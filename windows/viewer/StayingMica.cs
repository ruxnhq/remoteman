using Microsoft.UI.Composition;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

namespace Remoteman;

/// Mica that stays Mica when another window has the keyboard. WinUI's own MicaBackdrop follows
/// the window's activation and drops to a flat colour when it loses focus; this one keeps its
/// own configuration, always input-active, and only follows the theme.
public sealed class StayingMica : SystemBackdrop
{
    MicaController? _mica;
    SystemBackdropConfiguration? _config;

    protected override void OnTargetConnected(ICompositionSupportsSystemBackdrop target, XamlRoot root)
    {
        base.OnTargetConnected(target, root);
        if (!MicaController.IsSupported())
        {
            Log.Say("no Mica on this Windows: the pages keep their plain fill");
            return;
        }
        _config = new SystemBackdropConfiguration { IsInputActive = true };
        Follow(target, root);
        _mica = new MicaController();
        _mica.SetSystemBackdropConfiguration(_config);
        _mica.AddSystemBackdropTarget(target);
        Log.Say("mica: on, input-active always");
    }

    protected override void OnDefaultSystemBackdropConfigurationChanged(ICompositionSupportsSystemBackdrop target, XamlRoot root)
    {
        Follow(target, root);
    }

    void Follow(ICompositionSupportsSystemBackdrop target, XamlRoot root)
    {
        if (_config is null) return;
        var normal = GetDefaultSystemBackdropConfiguration(target, root);
        _config.Theme = normal.Theme;
        _config.IsHighContrast = normal.IsHighContrast;
        _config.IsInputActive = true;
    }

    protected override void OnTargetDisconnected(ICompositionSupportsSystemBackdrop target)
    {
        base.OnTargetDisconnected(target);
        if (_mica is not null)
        {
            _mica.RemoveSystemBackdropTarget(target);
            _mica.Dispose();
            _mica = null;
        }
    }
}
