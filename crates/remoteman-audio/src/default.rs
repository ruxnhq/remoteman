//! Which sink the desktop plays to. PipeWire keeps this in its "default" metadata, and the
//! session manager (WirePlumber) acts on it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use pipewire as pw;
use pw::metadata::Metadata;
use pw::types::ObjectType;

/// The sink the user chose, as `{"name":"<node.name>"}`. WirePlumber follows it: it moves
/// what is playing to that sink and saves the choice. This is the key `wpctl set-default`
/// writes.
const CONFIGURED_SINK: &str = "default.configured.audio.sink";

/// The sink in use right now, written by WirePlumber: the configured one if it exists, else
/// the best one there is. Only read here.
const CURRENT_SINK: &str = "default.audio.sink";

const JSON: &str = "Spa:String:JSON";

/// How long PipeWire gets to answer.
const TIMEOUT: Duration = Duration::from_secs(3);

/// A connection to PipeWire with its "default" metadata object and what that holds.
struct Defaults {
    // Dropped in this order: what came from the connection before the connection.
    _metadata_listener: pw::metadata::MetadataListener,
    metadata: Metadata,
    values: Rc<RefCell<HashMap<String, String>>>,
    core: pw::core::CoreRc,
    _context: pw::context::ContextRc,
    mainloop: pw::main_loop::MainLoopRc,
}

impl Defaults {
    fn connect() -> Result<Self> {
        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        let core = context.connect_rc(None).context("connect to PipeWire")?;
        let registry = core.get_registry_rc()?;

        // The values of the node-independent keys (subject 0), kept up to date while the loop
        // runs.
        let values = Rc::new(RefCell::new(HashMap::new()));
        let found: Rc<RefCell<Option<(Metadata, pw::metadata::MetadataListener)>>> = Rc::new(RefCell::new(None));
        let listener = registry
            .add_listener_local()
            .global({
                let (registry, found, values) = (registry.downgrade(), found.clone(), values.clone());
                move |global| {
                    let named_default = global.props.is_some_and(|p| p.get("metadata.name") == Some("default"));
                    if global.type_ != ObjectType::Metadata || !named_default || found.borrow().is_some() {
                        return;
                    }
                    let Some(registry) = registry.upgrade() else { return };
                    let Ok(metadata) = registry.bind::<Metadata, _>(global) else { return };
                    let values = values.clone();
                    let listener = metadata
                        .add_listener_local()
                        .property(move |subject, key, _, value| {
                            if subject == 0 {
                                let mut values = values.borrow_mut();
                                match (key, value) {
                                    (Some(key), Some(value)) => {
                                        values.insert(key.to_owned(), value.to_owned());
                                    }
                                    (Some(key), None) => {
                                        values.remove(key);
                                    }
                                    (None, _) => values.clear(),
                                }
                            }
                            0
                        })
                        .register();
                    *found.borrow_mut() = Some((metadata, listener));
                }
            })
            .register();

        // Once round for the list of objects, which binds the metadata; once more for its
        // values.
        roundtrip(&mainloop, &core)?;
        roundtrip(&mainloop, &core)?;
        drop(listener);
        let (metadata, metadata_listener) = found.borrow_mut().take().ok_or_else(|| anyhow!("PipeWire has no \"default\" metadata: is a session manager running?"))?;
        Ok(Self { metadata, _metadata_listener: metadata_listener, values, core, _context: context, mainloop })
    }

    fn get(&self, key: &str) -> Option<String> {
        self.values.borrow().get(key).cloned()
    }

    /// Write a key (or remove it, with `None`) and wait until PipeWire has taken it.
    fn set(&self, key: &str, value: Option<&str>) -> Result<()> {
        self.metadata.set_property(0, key, value.map(|_| JSON), value);
        roundtrip(&self.mainloop, &self.core)
    }
}

/// Run the loop until PipeWire has answered everything sent so far.
fn roundtrip(mainloop: &pw::main_loop::MainLoopRc, core: &pw::core::CoreRc) -> Result<()> {
    let done = Rc::new(Cell::new(false));
    let pending = core.sync(0).context("PipeWire sync")?;
    let _listener = core
        .add_listener_local()
        .done({
            let (done, mainloop) = (done.clone(), mainloop.clone());
            move |id, seq| {
                if id == pw::core::PW_ID_CORE && seq == pending {
                    done.set(true);
                    mainloop.quit();
                }
            }
        })
        .register();
    // Give up rather than wait forever on a PipeWire that doesn't answer.
    let timer = mainloop.loop_().add_timer({
        let mainloop = mainloop.clone();
        move |_| mainloop.quit()
    });
    timer.update_timer(Some(TIMEOUT), None).into_result().context("arm timer")?;
    mainloop.run();
    if !done.get() {
        bail!("PipeWire did not answer within {TIMEOUT:?}");
    }
    Ok(())
}

/// The `name` out of `{"name":"..."}`, which is all the default keys hold.
fn name_of(json: &str) -> Option<String> {
    let rest = json.split_once("\"name\"")?.1.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let mut name = String::new();
    let mut chars = rest.chars();
    loop {
        match chars.next()? {
            '"' => return Some(name),
            '\\' => name.push(chars.next()?),
            c => name.push(c),
        }
    }
}

fn json_of(name: &str) -> String {
    format!("{{\"name\":\"{}\"}}", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The `node.name` of the sink the desktop plays to right now, if there is one. Only reads.
pub fn default_sink() -> Result<Option<String>> {
    let defaults = Defaults::connect()?;
    Ok(defaults.get(CURRENT_SINK).as_deref().and_then(name_of))
}

/// The host's sink as the desktop's output, for as long as this lives.
///
/// It writes the user's configured default, as `wpctl set-default` does: WirePlumber then moves
/// the playing programs to the sink and keeps new ones on it. [`DefaultSink::restore`], or
/// dropping it, writes back what was configured before (or removes the key, if nothing was), and
/// the sound returns to where it was.
///
/// WirePlumber saves the configured default to disk. If the process dies without restoring, the
/// saved choice stays the host's sink: once that sink is gone WirePlumber falls back to the best
/// device it has, but what the user had chosen before is forgotten.
pub struct DefaultSink {
    name: String,
    /// The configured value from before, as it was written. `None` here: nothing was configured.
    /// The outer `None`: already restored.
    previous: Option<Option<String>>,
}

impl DefaultSink {
    /// Make the sink with this `node.name` (a [`crate::SpeakerSink`]'s) the default.
    pub fn set(name: &str) -> Result<Self> {
        let defaults = Defaults::connect()?;
        let previous = defaults.get(CONFIGURED_SINK);
        defaults.set(CONFIGURED_SINK, Some(&json_of(name)))?;
        Ok(Self { name: name.to_owned(), previous: Some(previous) })
    }

    /// Put back the default from before. Unless the user picked another one in the meantime:
    /// then theirs stays.
    pub fn restore(mut self) -> Result<()> {
        self.put_back()
    }

    fn put_back(&mut self) -> Result<()> {
        let Some(previous) = self.previous.take() else { return Ok(()) };
        let defaults = Defaults::connect()?;
        if defaults.get(CONFIGURED_SINK).as_deref().and_then(name_of).as_deref() != Some(self.name.as_str()) {
            return Ok(());
        }
        defaults.set(CONFIGURED_SINK, previous.as_deref())
    }
}

impl Drop for DefaultSink {
    fn drop(&mut self) {
        if let Err(e) = self.put_back() {
            eprintln!("remoteman-audio: restore the default sink: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_survive_the_json() {
        assert_eq!(name_of(r#"{"name":"alsa_output.usb-X-00.analog-stereo"}"#).as_deref(), Some("alsa_output.usb-X-00.analog-stereo"));
        assert_eq!(name_of(r#"{ "name": "remoteman" }"#).as_deref(), Some("remoteman"));
        assert_eq!(name_of("{}"), None);
        for name in ["remoteman", "a \"quoted\" one", "back\\slash"] {
            assert_eq!(name_of(&json_of(name)).as_deref(), Some(name));
        }
    }
}
