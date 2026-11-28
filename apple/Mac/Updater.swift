// Updates of the Mac viewer: looking for a newer release by itself (a minute after the app opens,
// then every 12 hours, and on Check Now), saying so in Settings, in the window's sidebar and once
// as a notification, and installing it with one click, or by itself when that is switched on and
// no session is running. Like the Windows host's updates, from remoteman's own download page:
// the disk image and the .sha256 beside it (UpdateCheck.swift), then the app inside it is
// checked to be signed with remoteman's Developer ID before it replaces this one.
//
// For a test without the window: `--update-check` looks and prints what it found; `--update-install`
// also installs. REMOTEMAN_PRETEND_VERSION=0.1.0 makes the app take itself for that version.
import AppKit
import CryptoKit
import Security
import UserNotifications

final class Updater: ObservableObject {
    /// Settings > General > Updates.
    static let lookKey = "updates.look"
    static let installKey = "updates.install"
    /// The version an update was installed from, for "Updated from…" once the new app opens.
    private static let fromKey = "updates.installedFrom"
    /// The version the notification has been shown for, so it comes once.
    private static let toldKey = "updates.told"
    /// The version whose notice the person put away in the sidebar.
    private static let putAwayKey = "updates.putAway"

    static let firstLook: TimeInterval = 60
    static let every: TimeInterval = 12 * 3600

    /// This app's version (CFBundleShortVersionString).
    let this: UpdateVersion
    @Published private(set) var state: UpdateState = .notYet
    /// The last time the download page was read.
    @Published private(set) var lastLooked: Date?
    /// This app came from an update, from that version (until the next update).
    @Published private(set) var updatedFrom: UpdateVersion?
    /// The last install didn't work (the state says why): the notice offers Try Again.
    @Published private(set) var installFailed = false
    @Published private(set) var putAway: String

    /// Installing now cuts no session (set by the app's model).
    var idle: () -> Bool = { true }

    private var release: UpdateRelease?
    /// The newer version found, also while an install of it failed.
    var offered: UpdateVersion? { release?.version }
    private var timer: Timer?
    /// The person asked for this install while a session was running: it is done when the session ends.
    private var afterSession = false
    /// Check for Updates… in the menu: the outcome is said in an alert.
    private var sayOutcome = false
    private let work = DispatchQueue(label: "remoteman.updates")

    init() {
        let pretend = ProcessInfo.processInfo.environment["REMOTEMAN_PRETEND_VERSION"].flatMap(UpdateVersion.init)
        let own = (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String).flatMap(UpdateVersion.init)
        this = pretend ?? own ?? UpdateVersion("0")!
        UserDefaults.standard.register(defaults: [Self.lookKey: true, Self.installKey: false])
        putAway = UserDefaults.standard.string(forKey: Self.putAwayKey) ?? ""
        if let from = UserDefaults.standard.string(forKey: Self.fromKey).flatMap(UpdateVersion.init), from < this {
            updatedFrom = from
        }
    }

    static var looks: Bool { UserDefaults.standard.bool(forKey: lookKey) }
    static var installs: Bool { looks && UserDefaults.standard.bool(forKey: installKey) }

    /// From the app's start. `automatic` is false for unattended runs, which look only when asked.
    func start(automatic: Bool) {
        let args = CommandLine.arguments
        if args.contains("--update-check") || args.contains("--update-install") { return test(install: args.contains("--update-install")) }
        guard automatic else { return }
        work.async { self.clean(keep: nil) }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.firstLook) { [weak self] in
            if Self.looks { self?.check() }
        }
        timer = Timer.scheduledTimer(withTimeInterval: Self.every, repeats: true) { [weak self] _ in
            if Self.looks { self?.check() }
        }
    }

    /// Looking by itself switched on or off in Settings.
    func lookingChanged() {
        if Self.looks, !state.busy, state.newer == nil { check() }
    }

    // MARK: Looking

    /// Read the download page, and say whether there is something newer than this app.
    func check() {
        guard !state.busy else { return }
        state = .checking
        work.async {
            let found = Result { try self.fetch(UpdateSource.page) }
                .mapError { UpdateFailure("remoteman.ruxn.com couldn't be read: \($0.localizedDescription)") }
                .flatMap { data in
                    UpdateCheck.newest(onPage: String(decoding: data, as: UTF8.self))
                }
            DispatchQueue.main.async { self.looked(found) }
        }
    }

    /// Check for Updates… in the app menu.
    func checkFromMenu() {
        sayOutcome = true
        if state.busy {
            if case .checking = state { return }
            return say(state.words(this: this), informative: nil)
        }
        check()
    }

    private func looked(_ found: Result<UpdateRelease, UpdateFailure>) {
        lastLooked = Date()
        switch found {
        case .failure(let f):
            state = .failed(f.words)
            installFailed = false
        case .success(let r) where r.version > this:
            release = r
            state = .available(r.version)
            installFailed = false
            tell(r.version)
            if Self.installs, idle(), !sayOutcome { return install(asked: false) }
        case .success(let r):
            release = nil
            state = .upToDate(r.version)
            installFailed = false
        }
        if sayOutcome {
            sayOutcome = false
            if let v = state.newer {
                askToInstall(v)
            } else {
                say(state.words(this: this), informative: nil)
            }
        }
    }

    /// The notification, once for each version. macOS asks the first time whether remoteman may.
    private func tell(_ v: UpdateVersion) {
        guard UserDefaults.standard.string(forKey: Self.toldKey) != v.description else { return }
        UserDefaults.standard.set(v.description, forKey: Self.toldKey)
        let center = UNUserNotificationCenter.current()
        center.requestAuthorization(options: [.alert]) { ok, _ in
            guard ok else { return }
            let content = UNMutableNotificationContent()
            content.title = "remoteman \(v) is available"
            content.body = Self.installs
                ? "It is installed by itself when no session is running."
                : "This Mac has \(self.this). Install it from the remoteman window or Settings."
            center.add(UNNotificationRequest(identifier: "update-\(v)", content: content, trigger: nil))
        }
    }

    /// The sidebar's notice put away for this version (Settings still shows it).
    func putAwayNotice() {
        guard let v = state.newer else { return }
        putAway = v.description
        UserDefaults.standard.set(putAway, forKey: Self.putAwayKey)
    }

    // MARK: Installing

    /// Whether this app can replace itself where it is, and why not.
    var whyCantInstall: String? {
        let here = Bundle.main.bundleURL
        if here.path.contains("/AppTranslocation/") || here.path.hasPrefix("/Volumes/") {
            return "remoteman is running from the disk image or a place macOS moved it to. Drag it to Applications, open it from there, and it can install updates."
        }
        let parent = here.deletingLastPathComponent().path
        if !FileManager.default.isWritableFile(atPath: parent) {
            return "This Mac's user can't change \(parent), so remoteman can't replace itself there."
        }
        return nil
    }

    /// Install, Try Again, or the menu's alert: as soon as no session is running.
    func requestInstall() {
        guard state.newer != nil || installFailed, !state.busy else { return }
        if idle() { install(asked: true) } else { afterSession = true }
    }

    /// The session ended: an install that waited for it, or one that is to happen by itself.
    func sessionEnded() {
        guard idle(), !state.busy, release != nil, case .available = state else { return }
        if afterSession || Self.installs {
            let asked = afterSession
            afterSession = false
            install(asked: asked)
        }
    }

    /// Download the disk image, check it, put its app in place of this one, and open it. When
    /// the person `asked` for it, a failure is also said in an alert.
    private func install(asked: Bool) {
        guard let r = release, !state.busy else { return }
        if let why = whyCantInstall {
            state = .failed(why)
            installFailed = true
            return
        }
        state = .downloading(r.version)
        installFailed = false
        work.async {
            do {
                let image = try self.download(r)
                DispatchQueue.main.async { self.state = .installing(r.version) }
                let staged = try self.stage(image, r)
                try self.swap(staged, version: r.version)
            } catch {
                let words = (error as? UpdateFailure)?.words ?? error.localizedDescription
                DispatchQueue.main.async {
                    self.state = .failed(words)
                    self.installFailed = true
                    if asked { self.say("remoteman \(r.version) wasn't installed", informative: words) }
                }
            }
        }
    }

    private var folder: URL {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        return caches.appendingPathComponent(UpdateSource.identifier).appendingPathComponent("Updates")
    }

    /// Unfinished downloads and ones that are no newer than this app go.
    private func clean(keep: String?) {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: folder.path)) ?? []
        for name in UpdateCheck.stale(names, current: this, keep: keep) {
            try? FileManager.default.removeItem(at: folder.appendingPathComponent(name))
        }
    }

    /// The disk image in the updates folder, its SHA-256 matching the one beside it on the site.
    private func download(_ r: UpdateRelease) throws -> URL {
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let sums = try fetch(r.sha256URL)
        let want = try UpdateCheck.sha256(of: String(decoding: sums, as: UTF8.self), file: r.file).get()
        let image = folder.appendingPathComponent(r.file)
        if FileManager.default.fileExists(atPath: image.path), (try? sha256(image)) == want { return image }
        let part = folder.appendingPathComponent(r.file + ".part")
        try? FileManager.default.removeItem(at: part)
        try fetch(r.url, to: part)
        let got = try sha256(part)
        guard got == want else {
            try? FileManager.default.removeItem(at: part)
            throw UpdateFailure("the downloaded \(r.file) isn't the one the site lists (SHA-256 \(got.prefix(12))…, not \(want.prefix(12))…)")
        }
        try? FileManager.default.removeItem(at: image)
        try FileManager.default.moveItem(at: part, to: image)
        clean(keep: r.file)
        return image
    }

    /// The new app copied out of the disk image, beside this one, once it is shown to be
    /// remoteman's: signed with its Developer ID, accepted by Gatekeeper, the version asked for.
    private func stage(_ image: URL, _ r: UpdateRelease) throws -> URL {
        let mount = FileManager.default.temporaryDirectory.appendingPathComponent("remoteman-update-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: mount, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: mount) }
        try run("/usr/bin/hdiutil", ["attach", image.path, "-nobrowse", "-readonly", "-noautoopen", "-mountpoint", mount.path], what: "the disk image couldn't be opened")
        defer { _ = try? run("/usr/bin/hdiutil", ["detach", mount.path, "-force"], what: "") }
        let app = mount.appendingPathComponent(UpdateSource.app)
        guard let info = Bundle(url: app)?.infoDictionary else { throw UpdateFailure("the disk image holds no \(UpdateSource.app)") }
        guard info["CFBundleIdentifier"] as? String == UpdateSource.identifier else {
            throw UpdateFailure("the app in the disk image is \(info["CFBundleIdentifier"] as? String ?? "unnamed"), not remoteman's viewer")
        }
        guard (info["CFBundleShortVersionString"] as? String).flatMap(UpdateVersion.init) == r.version else {
            throw UpdateFailure("the app in the disk image says it is \(info["CFBundleShortVersionString"] as? String ?? "no version"), not \(r.version)")
        }
        try checkSignature(app)
        try run("/usr/sbin/spctl", ["--assess", "--type", "execute", app.path], what: "macOS's Gatekeeper doesn't accept the app in the disk image")
        let staged = Bundle.main.bundleURL.deletingLastPathComponent().appendingPathComponent(".remoteman-\(r.version)-new.app")
        try? FileManager.default.removeItem(at: staged)
        try run("/usr/bin/ditto", [app.path, staged.path], what: "the new app couldn't be copied next to this one")
        try checkSignature(staged)
        return staged
    }

    /// Signed with remoteman's Developer ID, sealed, nothing in it changed.
    private func checkSignature(_ app: URL) throws {
        let requirement = "anchor apple generic and identifier \"\(UpdateSource.identifier)\" and certificate leaf[subject.OU] = \"\(UpdateSource.team)\""
        var code: SecStaticCode?
        var req: SecRequirement?
        guard SecStaticCodeCreateWithPath(app as CFURL, [], &code) == errSecSuccess, let code,
            SecRequirementCreateWithString(requirement as CFString, [], &req) == errSecSuccess, let req
        else { throw UpdateFailure("the app's signature couldn't be read") }
        // kSecCSCheckAllArchitectures | kSecCSCheckNestedCode | kSecCSStrictValidate
        let status = SecStaticCodeCheckValidity(code, SecCSFlags(rawValue: 1 << 0 | 1 << 3 | 1 << 4), req)
        guard status == errSecSuccess else {
            let why = SecCopyErrorMessageString(status, nil) as String? ?? "error \(status)"
            throw UpdateFailure("the app isn't signed with remoteman's Developer ID (\(UpdateSource.team)): \(why)")
        }
    }

    /// Once this app has quit, a small shell puts the new one in its place and opens it; if the
    /// new one can't be put there, the old one goes back.
    private func swap(_ staged: URL, version: UpdateVersion) throws {
        let here = Bundle.main.bundleURL
        let old = here.deletingLastPathComponent().appendingPathComponent(".remoteman-\(this)-old.app")
        let log = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0].appendingPathComponent("Logs/remoteman-update.log")
        let script = """
            pid=$1; here=$2; new=$3; old=$4
            echo "$(date): replacing $here with $new once process $pid has quit"
            i=0
            while kill -0 "$pid" 2>/dev/null; do sleep 0.2; i=$((i+1)); [ $i -gt 600 ] && { echo "it didn't quit"; rm -rf "$new"; exit 1; }; done
            rm -rf "$old"
            mv "$here" "$old" || { echo "the old app couldn't be moved"; rm -rf "$new"; exit 1; }
            if mv "$new" "$here"; then rm -rf "$old"; echo "done"; else echo "the new app couldn't be moved, the old one goes back"; mv "$old" "$here"; fi
            open "$here"
            """
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", script, "sh", String(ProcessInfo.processInfo.processIdentifier), here.path, staged.path, old.path]
        FileManager.default.createFile(atPath: log.path, contents: nil)
        let out = try? FileHandle(forWritingTo: log)
        out?.seekToEndOfFile()
        p.standardOutput = out
        p.standardError = out
        p.standardInput = FileHandle.nullDevice
        try p.run()
        UserDefaults.standard.set(this.description, forKey: Self.fromKey)
        UserDefaults.standard.synchronize()
        DispatchQueue.main.async { NSApp.terminate(nil) }
    }

    // MARK: Asking and saying

    /// The menu's check found something: install it now?
    private func askToInstall(_ v: UpdateVersion) {
        let a = NSAlert()
        a.messageText = "remoteman \(v) is available"
        a.informativeText = whyCantInstall ?? "This Mac has \(this). Installing downloads it from remoteman.ruxn.com, and remoteman quits and opens again."
        if whyCantInstall == nil {
            a.addButton(withTitle: "Install")
            a.addButton(withTitle: "Later")
        } else {
            a.addButton(withTitle: "OK")
        }
        if a.runModal() == .alertFirstButtonReturn, whyCantInstall == nil { askedInstall?() }
    }

    /// Install, as the person asked: the app's model asks first when a session is running.
    var askedInstall: (() -> Void)?

    private func say(_ message: String, informative: String?) {
        guard !Launch.automated else { return }
        let a = NSAlert()
        a.messageText = message
        if let informative { a.informativeText = informative }
        a.runModal()
    }

    // MARK: Plumbing

    private func fetch(_ url: URL) throws -> Data {
        var req = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 30)
        req.setValue("remoteman-viewer-macos/\(this)", forHTTPHeaderField: "User-Agent")
        var result: Result<Data, Error> = .failure(UpdateFailure("no answer from \(url.host ?? "the site")"))
        let done = DispatchSemaphore(value: 0)
        URLSession.shared.dataTask(with: req) { data, response, error in
            if let error {
                result = .failure(error)
            } else if let http = response as? HTTPURLResponse, http.statusCode != 200 {
                result = .failure(UpdateFailure("\(url.absoluteString) answered \(http.statusCode)"))
            } else {
                result = .success(data ?? Data())
            }
            done.signal()
        }.resume()
        done.wait()
        return try result.get()
    }

    private func fetch(_ url: URL, to file: URL) throws {
        var req = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 60)
        req.setValue("remoteman-viewer-macos/\(this)", forHTTPHeaderField: "User-Agent")
        var failure: Error?
        let done = DispatchSemaphore(value: 0)
        URLSession.shared.downloadTask(with: req) { temp, response, error in
            defer { done.signal() }
            if let error { failure = error; return }
            if let http = response as? HTTPURLResponse, http.statusCode != 200 {
                failure = UpdateFailure("\(url.absoluteString) answered \(http.statusCode)")
                return
            }
            guard let temp else { failure = UpdateFailure("the download of \(url.lastPathComponent) came to nothing"); return }
            do { try FileManager.default.moveItem(at: temp, to: file) } catch { failure = error }
        }.resume()
        done.wait()
        if let failure { throw (failure as? UpdateFailure) ?? UpdateFailure("\(url.lastPathComponent) couldn't be downloaded: \(failure.localizedDescription)") }
    }

    private func sha256(_ file: URL) throws -> String {
        let h = try FileHandle(forReadingFrom: file)
        defer { try? h.close() }
        var hasher = SHA256()
        while let chunk = try h.read(upToCount: 1 << 20), !chunk.isEmpty { hasher.update(data: chunk) }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    /// A tool's exit status 0, or `what` with its last words.
    @discardableResult
    private func run(_ tool: String, _ args: [String], what: String) throws -> String {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = args
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        p.standardInput = FileHandle.nullDevice
        try p.run()
        let out = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        p.waitUntilExit()
        guard p.terminationStatus == 0 else {
            let last = out.split(whereSeparator: \.isNewline).last.map(String.init) ?? "exit status \(p.terminationStatus)"
            throw UpdateFailure("\(what) (\(URL(fileURLWithPath: tool).lastPathComponent): \(last))")
        }
        return out
    }

    // MARK: Test

    /// `--update-check` and `--update-install`: what was found is printed, then the app quits
    /// (an install opens the new app instead).
    private func test(install: Bool) {
        print("update-test: this app is \(this), at \(Bundle.main.bundleURL.path)")
        state = .checking
        work.async {
            let found = Result { try self.fetch(UpdateSource.page) }
                .mapError { UpdateFailure("remoteman.ruxn.com couldn't be read: \($0.localizedDescription)") }
                .flatMap { UpdateCheck.newest(onPage: String(decoding: $0, as: UTF8.self)) }
            switch found {
            case .failure(let f):
                print("update-test: failed: \(f.words)")
                exit(1)
            case .success(let r):
                print("update-test: newest on the page is \(r.version) (\(r.url.absoluteString))")
                guard r.version > self.this else { print("update-test: no update"); exit(0) }
                guard install else { print("update-test: update available"); exit(0) }
                if let why = self.whyCantInstall { print("update-test: can't install: \(why)"); exit(1) }
                do {
                    let image = try self.download(r)
                    print("update-test: downloaded \(image.path), SHA-256 matches")
                    let staged = try self.stage(image, r)
                    print("update-test: signature and Gatekeeper accepted, staged at \(staged.path)")
                    try self.swap(staged, version: r.version)
                    print("update-test: quitting; the new app opens in a moment (log in ~/Library/Logs/remoteman-update.log)")
                } catch {
                    print("update-test: failed: \((error as? UpdateFailure)?.words ?? error.localizedDescription)")
                    exit(1)
                }
            }
        }
    }
}
