// LavaLampBackground — three radial-gradient ellipses drifting behind the UI, the app's visual signature (matches macOS).
//
// Built on Microsoft.UI.Composition rather than Win2D: CanvasAnimatedControl fast-fails in CoreMessagingXP
// (0xC000027B) on Windows 11 build 26200+, and a Gaussian blur effect would need Win2D too. A radial gradient
// gives the same soft falloff in hardware.
//
// Each ellipse drifts on sin/cos paths with the macOS time multipliers (0.20/0.23, 0.15/0.18, 0.10/0.12).
// Animation pauses while the host is occluded, runs at half rate under reduced motion, and is replaced by a
// solid Bg1 when transparency effects are off or high contrast is on.

using System;
using System.Numerics;
using Microsoft.UI;
using Microsoft.UI.Composition;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Hosting;
using Windows.UI;

namespace FileID.Theme.Motion;

public sealed class LavaLampBackground : Control
{
    private SpriteVisual? _root;
    private SpriteVisual? _goldVisual;
    private SpriteVisual? _orangeVisual;
    private SpriteVisual? _darkVisual;
    private CompositionColorBrush? _baseBrush;
    private bool _animationsRunning;

    public LavaLampBackground()
    {
        DefaultStyleKey = typeof(LavaLampBackground);
        IsHitTestVisible = false;
        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
        SizeChanged += OnSizeChanged;
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        if (_root is not null) return;
        BuildVisualTree();
        StartAnimations();
        ReducedMotion.Instance.PropertyChanged += OnReducedMotionChanged;
        TransparencyEffects.Instance.PropertyChanged += OnTransparencyChanged;
        LavaLampMode.Instance.PropertyChanged += OnTransparencyChanged;
        if (XamlRoot is { } root)
        {
            root.Changed += OnXamlRootChanged;
            ApplyVisibility();
        }
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        ReducedMotion.Instance.PropertyChanged -= OnReducedMotionChanged;
        TransparencyEffects.Instance.PropertyChanged -= OnTransparencyChanged;
        LavaLampMode.Instance.PropertyChanged -= OnTransparencyChanged;
        if (XamlRoot is { } root)
        {
            root.Changed -= OnXamlRootChanged;
        }
        // Stop the debounce timer so no Tick fires after unload; OnSizeChanged recreates it on the next resize.
        _resizeDebounce?.Stop();
        _resizeDebounce = null;
        StopAnimations();
        if (_root is not null)
        {
            ElementCompositionPreview.SetElementChildVisual(this, null);
            _root.Dispose();
            _root = null;
            _goldVisual = null;
            _orangeVisual = null;
            _darkVisual = null;
            _baseBrush = null;
        }
    }

    private DispatcherTimer? _resizeDebounce;

    private void OnSizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_root is null) return;
        _root.Size = new Vector2((float)ActualWidth, (float)ActualHeight);
        // Debounce so a resize drag does not restart the three expression animations 30+ times a second.
        if (_resizeDebounce is null)
        {
            _resizeDebounce = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(100) };
            _resizeDebounce.Tick += (_, _) =>
            {
                _resizeDebounce!.Stop();
                StopAnimations();
                // Restart via ApplyVisibility: StartAnimations() alone would resume drift while occluded.
                ApplyVisibility();
            };
        }
        _resizeDebounce.Stop();
        _resizeDebounce.Start();
    }

    private void OnXamlRootChanged(XamlRoot sender, XamlRootChangedEventArgs args)
        => ApplyVisibility();

    private void OnReducedMotionChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        // The rate scale is read in StartAnimations(), so restart. The OS fires this off-thread; going through
        // ApplyVisibility avoids resuming drift while occluded.
        DispatcherQueue.TryEnqueue(() =>
        {
            StopAnimations();
            ApplyVisibility();
        });
    }

    private void OnTransparencyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        DispatcherQueue.TryEnqueue(() =>
        {
            try
            {
                StopAnimations();
                ApplyVisibility();
            }
            catch (Exception ex)
            {
                System.Diagnostics.Trace.TraceError($"LavaLampBackground transparency change failed: {ex}");
            }
        });
    }

    private void ApplyVisibility()
    {
        bool solid = TransparencyEffects.Instance.IsSolid;
        if (_goldVisual is not null) _goldVisual.IsVisible = !solid;
        if (_orangeVisual is not null) _orangeVisual.IsVisible = !solid;
        if (_darkVisual is not null) _darkVisual.IsVisible = !solid;
        if (_baseBrush is not null)
        {
            _baseBrush.Color = solid
                ? ResolveColor("Bg1Color", Color.FromArgb(0xFF, 0x0B, 0x0B, 0x0C))
                : ResolveColor("LavaLampBaseColor", Color.FromArgb(0xFF, 0x14, 0x14, 0x14));
        }
        bool visible = (XamlRoot?.IsHostVisible ?? true) && !solid && !LavaLampMode.Instance.IsOff;
        if (visible && !_animationsRunning) StartAnimations();
        else if (!visible && _animationsRunning) StopAnimations();
    }

    private void BuildVisualTree()
    {
        var compositor = ElementCompositionPreview.GetElementVisual(this).Compositor;

        _root = compositor.CreateSpriteVisual();
        _root.Size = new Vector2((float)Math.Max(1, ActualWidth), (float)Math.Max(1, ActualHeight));

        _baseBrush = compositor.CreateColorBrush(
            ResolveColor("LavaLampBaseColor", Color.FromArgb(0xFF, 0x14, 0x14, 0x14)));
        _root.Brush = _baseBrush;

        _goldVisual = CreateEllipseVisual(compositor, 800f,
            ResolveColor("LavaLampGoldEllipseColor", Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00)),
            centerOpacity: 0.40f);
        _orangeVisual = CreateEllipseVisual(compositor, 600f,
            ResolveColor("LavaLampOrangeEllipseColor", Color.FromArgb(0xFF, 0xFF, 0x66, 0x00)),
            centerOpacity: 0.30f);
        _darkVisual = CreateEllipseVisual(compositor, 1000f,
            ResolveColor("LavaLampDarkEllipseColor", Color.FromArgb(0xFF, 0x0D, 0x0D, 0x0D)),
            centerOpacity: 0.55f);

        CenterVisual(_goldVisual, _root.Size);
        CenterVisual(_orangeVisual, _root.Size);
        CenterVisual(_darkVisual, _root.Size);

        _root.Children.InsertAtTop(_goldVisual);
        _root.Children.InsertAtTop(_orangeVisual);
        _root.Children.InsertAtTop(_darkVisual);

        ElementCompositionPreview.SetElementChildVisual(this, _root);
    }

    private static void CenterVisual(SpriteVisual visual, Vector2 parent)
        => visual.Offset = new Vector3(parent.X * 0.5f - visual.Size.X * 0.5f, parent.Y * 0.5f - visual.Size.Y * 0.5f, 0f);

    private static SpriteVisual CreateEllipseVisual(Compositor c, float diameter, Color color, float centerOpacity)
    {
        var v = c.CreateSpriteVisual();
        v.Size = new Vector2(diameter, diameter);
        var radial = c.CreateRadialGradientBrush();
        var center = Color.FromArgb((byte)Math.Round(centerOpacity * 255f), color.R, color.G, color.B);
        var edge = Color.FromArgb(0, color.R, color.G, color.B);
        radial.ColorStops.Add(c.CreateColorGradientStop(0.0f, center));
        radial.ColorStops.Add(c.CreateColorGradientStop(1.0f, edge));
        radial.EllipseCenter = new Vector2(0.5f, 0.5f);
        radial.EllipseRadius = new Vector2(0.5f, 0.5f);
        v.Brush = radial;
        return v;
    }

    private void StartAnimations()
    {
        if (_root is null || _goldVisual is null || _orangeVisual is null || _darkVisual is null) return;
        var compositor = _root.Compositor;
        var width = _root.Size.X;
        var height = _root.Size.Y;
        if (width <= 0 || height <= 0) return;

        // Reduced motion doubles the periods (slower drift); the loop still runs so the screen never feels dead.
        float rateScale = ReducedMotion.Instance.IsReduced ? 2f : 1f;

        AnimateOffset(compositor, _goldVisual, 800f, width, height,
            xPeriod: TimeSpan.FromSeconds(2 * Math.PI / 0.20 * rateScale),
            yPeriod: TimeSpan.FromSeconds(2 * Math.PI / 0.23 * rateScale),
            xAmplitude: 0.30f, yAmplitude: 0.30f);
        AnimateOffset(compositor, _orangeVisual, 600f, width, height,
            xPeriod: TimeSpan.FromSeconds(2 * Math.PI / 0.15 * rateScale),
            yPeriod: TimeSpan.FromSeconds(2 * Math.PI / 0.18 * rateScale),
            xAmplitude: 0.40f, yAmplitude: 0.40f);
        AnimateOffset(compositor, _darkVisual, 1000f, width, height,
            xPeriod: TimeSpan.FromSeconds(2 * Math.PI / 0.10 * rateScale),
            yPeriod: TimeSpan.FromSeconds(2 * Math.PI / 0.12 * rateScale),
            xAmplitude: 0.20f, yAmplitude: 0.20f);

        _animationsRunning = true;
    }

    private void StopAnimations()
    {
        if (!_animationsRunning) return;
        StopVisualAnimations(_goldVisual);
        StopVisualAnimations(_orangeVisual);
        StopVisualAnimations(_darkVisual);
        _animationsRunning = false;
    }

    private static void StopVisualAnimations(SpriteVisual? visual)
    {
        if (visual is null) return;
        visual.StopAnimation("Offset");
        // The phase oscillators live on Properties; stop them too or they keep evaluating after a restart.
        visual.Properties.StopAnimation("xPhase");
        visual.Properties.StopAnimation("yPhase");
    }

    /// <summary>
    /// Drives the visual's Offset from two looping phase scalars (xPhase, yPhase) through an ExpressionAnimation computing
    /// <c>Sin(xPhase) * xSwing</c> and <c>Cos(yPhase) * ySwing</c>. The compositor evaluates it every vsync, so motion is smooth at any refresh rate.
    /// </summary>
    private static void AnimateOffset(
        Compositor compositor,
        SpriteVisual visual,
        float diameter,
        float parentWidth,
        float parentHeight,
        TimeSpan xPeriod,
        TimeSpan yPeriod,
        float xAmplitude,
        float yAmplitude)
    {
        var halfDiameter = diameter * 0.5f;
        var centerX = parentWidth * 0.5f - halfDiameter;
        var centerY = parentHeight * 0.5f - halfDiameter;
        var xSwing = parentWidth * xAmplitude;
        var ySwing = parentHeight * yAmplitude;

        // The phases run 0 to 2π over each period and live on the visual's Properties, so they are released with it.
        var props = visual.Properties;
        props.InsertScalar("xPhase", 0f);
        props.InsertScalar("yPhase", 0f);

        var twoPi = (float)(2.0 * Math.PI);

        var xPhaseAnim = compositor.CreateScalarKeyFrameAnimation();
        xPhaseAnim.Duration = xPeriod;
        xPhaseAnim.IterationBehavior = AnimationIterationBehavior.Forever;
        // Linear easing keeps dPhase/dt constant, so Sin(phase) is a true sine wave.
        var linear = compositor.CreateLinearEasingFunction();
        xPhaseAnim.InsertKeyFrame(0f, 0f, linear);
        xPhaseAnim.InsertKeyFrame(1f, twoPi, linear);
        props.StartAnimation("xPhase", xPhaseAnim);

        var yPhaseAnim = compositor.CreateScalarKeyFrameAnimation();
        yPhaseAnim.Duration = yPeriod;
        yPhaseAnim.IterationBehavior = AnimationIterationBehavior.Forever;
        yPhaseAnim.InsertKeyFrame(0f, 0f, linear);
        yPhaseAnim.InsertKeyFrame(1f, twoPi, linear);
        props.StartAnimation("yPhase", yPhaseAnim);

        var offsetExpr = compositor.CreateExpressionAnimation(
            "Vector3(centerX + Sin(props.xPhase) * xSwing, " +
            "centerY + Cos(props.yPhase) * ySwing, 0)");
        offsetExpr.SetReferenceParameter("props", props);
        offsetExpr.SetScalarParameter("centerX", centerX);
        offsetExpr.SetScalarParameter("centerY", centerY);
        offsetExpr.SetScalarParameter("xSwing", xSwing);
        offsetExpr.SetScalarParameter("ySwing", ySwing);
        visual.StartAnimation("Offset", offsetExpr);
    }

    private static Color ResolveColor(string key, Color fallback)
    {
        if (Application.Current?.Resources[key] is Color c) return c;
        return fallback;
    }
}
