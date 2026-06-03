// LavaLampBackground — three blurred ellipses drifting under a translucent
// overlay. The user's favorite touch; the visual signature of FileID.
//
// Built on Microsoft.UI.Composition (no Win2D). `CanvasAnimatedControl`
// fast-fails in CoreMessagingXP (exception 0xC000027B) on Windows 11
// build 26200+; Composition is the correct primitive anyway —
// GPU-accelerated, vsync-driven by DWM, no extra render target.
//
// Falloff shape: radial gradient. Microsoft.UI.Composition's effect graph
// would need a Win2D `IGraphicsEffect` source for GaussianBlurEffect,
// which would re-introduce the CoreMessagingXP fault. The radial gradient
// is a true hardware-rendered soft falloff that reads near-identical.
//
// Design:
//   * Three SpriteVisuals (gold, orange, dark) sized to a generous max
//     diameter. CompositionRadialGradientBrush gives each one a soft
//     falloff from `color @ centerOpacity` to `color @ 0` at the edge —
//     visually equivalent to a sharp-edged ellipse + gaussian blur, but
//     done entirely with hardware compositing.
//   * Position animates via Vector3KeyFrameAnimation on Visual.Offset.
//     Each ellipse uses sin/cos with the macOS time multipliers
//     (0.20/0.23, 0.15/0.18, 0.10/0.12).
//   * The whole composition lives behind everything else (ZIndex
//     equivalent: stacked first in MainWindow's Grid).
//   * Pause when occluded: XamlRoot.Changed + IsHostVisible.
//   * Reduced motion: halve the time multipliers (animations slow down).
//
// 1:1 visual reference: platforms/apple/app/Sources/FileID/Theme/LavaLampBackground.swift.

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
        if (XamlRoot is { } root)
        {
            root.Changed += OnXamlRootChanged;
            ApplyVisibility();
        }
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        ReducedMotion.Instance.PropertyChanged -= OnReducedMotionChanged;
        if (XamlRoot is { } root)
        {
            root.Changed -= OnXamlRootChanged;
        }
        // Stop + release the resize-debounce timer so no Tick fires post-unload
        // and the timer stops rooting the control (#25). OnSizeChanged recreates
        // it lazily on the next resize, so a control reload stays correct.
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
        }
    }

    private DispatcherTimer? _resizeDebounce;

    private void OnSizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_root is null) return;
        _root.Size = new Vector2((float)ActualWidth, (float)ActualHeight);
        // Debounce 100 ms so a fast resize drag doesn't thrash 3
        // ExpressionAnimations 30+ times per second. The Vector2 size on
        // _root above takes effect immediately (visuals don't blur);
        // only the sin/cos amplitudes need the restart.
        if (_resizeDebounce is null)
        {
            _resizeDebounce = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(100) };
            _resizeDebounce.Tick += (_, _) =>
            {
                _resizeDebounce!.Stop();
                StopAnimations();
                // Restart through ApplyVisibility so visibility stays the
                // single source of truth — an unconditional StartAnimations()
                // would resume drift while occluded/minimized, defeating the
                // XamlRoot.IsHostVisible occlusion pause.
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
        // The rate scale is only read inside StartAnimations(); a restart
        // applies the new value. Marshal to the UI thread (the OS fires this
        // off-thread) and route the restart through ApplyVisibility so we
        // don't resume drift while occluded.
        DispatcherQueue.TryEnqueue(() =>
        {
            StopAnimations();
            ApplyVisibility();
        });
    }

    private void ApplyVisibility()
    {
        bool visible = XamlRoot?.IsHostVisible ?? true;
        if (visible && !_animationsRunning) StartAnimations();
        else if (!visible && _animationsRunning) StopAnimations();
    }

    private void BuildVisualTree()
    {
        var compositor = ElementCompositionPreview.GetElementVisual(this).Compositor;

        _root = compositor.CreateSpriteVisual();
        _root.Size = new Vector2((float)Math.Max(1, ActualWidth), (float)Math.Max(1, ActualHeight));

        // Solid base: #141414 (Color(white: 0.08) on macOS).
        _root.Brush = compositor.CreateColorBrush(
            ResolveColor("LavaLampBaseColor", Color.FromArgb(0xFF, 0x14, 0x14, 0x14)));

        // Three soft-edge ellipses. The size is the max diameter the
        // composition needs; the radial brush handles the falloff.
        _goldVisual = CreateEllipseVisual(compositor, 800f,
            ResolveColor("LavaLampGoldEllipseColor", Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00)),
            centerOpacity: 0.40f);
        _orangeVisual = CreateEllipseVisual(compositor, 600f,
            ResolveColor("LavaLampOrangeEllipseColor", Color.FromArgb(0xFF, 0xFF, 0x66, 0x00)),
            centerOpacity: 0.30f);
        _darkVisual = CreateEllipseVisual(compositor, 1000f,
            ResolveColor("LavaLampDarkEllipseColor", Color.FromArgb(0xFF, 0x0D, 0x0D, 0x0D)),
            centerOpacity: 0.55f);

        _root.Children.InsertAtTop(_goldVisual);
        _root.Children.InsertAtTop(_orangeVisual);
        _root.Children.InsertAtTop(_darkVisual);

        ElementCompositionPreview.SetElementChildVisual(this, _root);
    }

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

        // Reduced-motion: halve the time rate (longer-period animations
        // → slower drift; the loop still runs so the screen never feels
        // dead).
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
        // Phase oscillators live on Properties; stop them too so the
        // ExpressionAnimation doesn't keep evaluating against drifting
        // phase values after we re-enter StartAnimations on resize.
        visual.Properties.StopAnimation("xPhase");
        visual.Properties.StopAnimation("yPhase");
    }

    /// <summary>
    /// Animates a SpriteVisual's Offset using two parallel scalar phase
    /// oscillators (xPhase, yPhase) on a CompositionPropertySet, fed
    /// through an ExpressionAnimation that computes
    /// <c>Sin(xPhase) * xSwing</c> / <c>Cos(yPhase) * ySwing</c>.
    ///
    /// This is true GPU-continuous motion: the compositor evaluates the
    /// expression every vsync, so the visual moves along a perfect sine
    /// curve at 60 Hz / 120 Hz / whatever the display runs. The previous
    /// implementation sampled sin/cos into 30 keyframes and let
    /// Composition piecewise-linearly interpolate between them — visible
    /// chop, especially on slow drifts where each segment lasts ~1 sec.
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

        // Drive two scalar phase variables 0 → 2π over xPeriod / yPeriod.
        // CompositionPropertySet survives the lifetime of the visual; we
        // attach it to the visual's Properties so disposal is automatic.
        var props = visual.Properties;
        props.InsertScalar("xPhase", 0f);
        props.InsertScalar("yPhase", 0f);

        var twoPi = (float)(2.0 * Math.PI);

        var xPhaseAnim = compositor.CreateScalarKeyFrameAnimation();
        xPhaseAnim.Duration = xPeriod;
        xPhaseAnim.IterationBehavior = AnimationIterationBehavior.Forever;
        // Linear ease so dPhase/dt is constant — Sin(linearly-ticking
        // phase) gives a perfect sine wave.
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
