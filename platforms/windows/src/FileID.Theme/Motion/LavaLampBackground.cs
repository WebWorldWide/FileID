// LavaLampBackground — three blurred ellipses drifting under a translucent
// overlay. The user's favorite touch; the visual signature of FileID.
//
// V14.6: rewritten on Microsoft.UI.Composition (no Win2D). The previous
// `CanvasAnimatedControl` implementation fast-failed in CoreMessagingXP
// (exception 0xC000027B) on Windows 11 build 26200+. Composition is the
// correct primitive anyway: GPU-accelerated, vsync-driven by DWM, no
// extra render target, no Win2D dep.
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
        if (XamlRoot is { } root)
        {
            root.Changed += OnXamlRootChanged;
            ApplyVisibility();
        }
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        if (XamlRoot is { } root)
        {
            root.Changed -= OnXamlRootChanged;
        }
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

    private void OnSizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_root is null) return;
        _root.Size = new Vector2((float)ActualWidth, (float)ActualHeight);
        // Re-start animations so the sin/cos amplitudes pick up the new
        // size; cheap (the animations are GPU resident).
        StopAnimations();
        StartAnimations();
    }

    private void OnXamlRootChanged(XamlRoot sender, XamlRootChangedEventArgs args)
        => ApplyVisibility();

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
        _goldVisual?.StopAnimation("Offset");
        _orangeVisual?.StopAnimation("Offset");
        _darkVisual?.StopAnimation("Offset");
        _animationsRunning = false;
    }

    /// <summary>
    /// Animates a SpriteVisual's Offset on a sin/cos loop. The
    /// `xAmplitude` / `yAmplitude` are fractions of the parent
    /// width/height, matching the macOS reference's 0.30/0.40/0.20
    /// scale factors.
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

        var loopPeriod = TimeSpan.FromTicks(Math.Max(xPeriod.Ticks, yPeriod.Ticks));

        // Vector3 keyframe animation; we sample sin/cos at 30 frames over
        // the loop period. Composition interpolates between frames on
        // the GPU, so 30 keyframes is more than enough for a smooth drift
        // and keeps the animation tree small.
        const int frameCount = 30;
        var anim = compositor.CreateVector3KeyFrameAnimation();
        anim.Duration = loopPeriod;
        anim.IterationBehavior = AnimationIterationBehavior.Forever;
        for (int i = 0; i <= frameCount; i++)
        {
            float progress = (float)i / frameCount;
            // Sample on full 2π so the loop closes cleanly.
            double angle = progress * 2.0 * Math.PI;
            float x = centerX + (float)Math.Sin(angle) * xSwing;
            float y = centerY + (float)Math.Cos(angle * (yPeriod.TotalSeconds / xPeriod.TotalSeconds)) * ySwing;
            anim.InsertKeyFrame(progress, new Vector3(x, y, 0f));
        }
        visual.StartAnimation("Offset", anim);
    }

    private static Color ResolveColor(string key, Color fallback)
    {
        if (Application.Current?.Resources[key] is Color c) return c;
        return fallback;
    }
}
