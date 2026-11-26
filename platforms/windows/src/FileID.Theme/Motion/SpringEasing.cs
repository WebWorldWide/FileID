// Wraps the Composition spring animation so call sites pass a SwiftUI-style response and dampingFraction.
// response maps to Period (seconds) and dampingFraction to DampingRatio; the compositor runs the physics.
// Callers skip the animation and snap (SetScalar) when ReducedMotion.Instance.IsReduced.

using Microsoft.UI.Composition;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Hosting;

namespace FileID.Theme.Motion;

public static class SpringEasing
{
    /// <summary>
    /// Springs a scalar Composition property (Translation.X, Opacity, Scale.X, ...) toward <paramref name="final"/>.
    /// The current value is the start, since natural-motion animations take no "from".
    /// </summary>
    public static void AnimateScalar(
        UIElement element,
        string propertyName,
        float final,
        double response,
        double dampingFraction,
        float initialVelocity = 0f)
    {
        var visual = ElementCompositionPreview.GetElementVisual(element);
        var compositor = visual.Compositor;

        var spring = compositor.CreateSpringScalarAnimation();
        spring.FinalValue = final;
        spring.Period = TimeSpan.FromSeconds(response);
        spring.DampingRatio = (float)dampingFraction;
        spring.InitialVelocity = initialVelocity;

        visual.StartAnimation(propertyName, spring);
    }

    /// <summary>Animates the element's Translation (post-layout offset) by a delta.</summary>
    public static void AnimateTranslation(
        UIElement element,
        float deltaX,
        float deltaY,
        double response,
        double dampingFraction)
    {
        ElementCompositionPreview.SetIsTranslationEnabled(element, true);
        AnimateScalar(element, "Translation.X", deltaX, response, dampingFraction);
        AnimateScalar(element, "Translation.Y", deltaY, response, dampingFraction);
    }

    /// <summary>Animates uniform scale around the element's center.</summary>
    public static void AnimateScale(
        UIElement element,
        float finalScale,
        double response,
        double dampingFraction)
    {
        var visual = ElementCompositionPreview.GetElementVisual(element);
        // GetElementVisual leaves visual.Size at (0,0), which anchored the scale at the top-left; use the laid-out size.
        if (element is FrameworkElement fe)
        {
            visual.CenterPoint = new System.Numerics.Vector3(
                (float)(fe.ActualWidth / 2), (float)(fe.ActualHeight / 2), 0);
        }
        else
        {
            var size = visual.Size;
            visual.CenterPoint = new System.Numerics.Vector3(size.X / 2, size.Y / 2, 0);
        }
        AnimateScalar(element, "Scale.X", finalScale, response, dampingFraction);
        AnimateScalar(element, "Scale.Y", finalScale, response, dampingFraction);
    }

    /// <summary>Springs the element's uniform scale with a token pair, or snaps to it when Windows is set to reduce motion.</summary>
    public static void SpringScale(UIElement element, float finalScale, Tokens tokens)
    {
        if (!ReducedMotion.Instance.IsReduced)
        {
            AnimateScale(element, finalScale, tokens.Response, tokens.DampingFraction);
            return;
        }
        SetScalar(element, "Scale.X", finalScale);
        SetScalar(element, "Scale.Y", finalScale);
    }

    /// <summary>Animates opacity from its current value to <paramref name="final"/>.</summary>
    public static void AnimateOpacity(
        UIElement element,
        float final,
        double response,
        double dampingFraction)
        => AnimateScalar(element, "Opacity", final, response, dampingFraction);

    /// <summary>Snaps a scalar property without animation, for reduced motion or an immediate final state.</summary>
    public static void SetScalar(UIElement element, string propertyName, float value)
    {
        var visual = ElementCompositionPreview.GetElementVisual(element);
        // Clear any in-flight spring before setting the value directly.
        visual.StopAnimation(propertyName);
        switch (propertyName)
        {
            case "Opacity": visual.Opacity = value; break;
            case "Scale.X": visual.Scale = new System.Numerics.Vector3(value, visual.Scale.Y, visual.Scale.Z); break;
            case "Scale.Y": visual.Scale = new System.Numerics.Vector3(visual.Scale.X, value, visual.Scale.Z); break;
            case "Translation.X":
            case "Translation.Y":
                // Translation is not a Visual property, so there is no direct snap; no consumer needs one yet.
                System.Diagnostics.Debug.WriteLine(
                    $"SpringEasing.SetScalar({propertyName}): direct set not supported for Translation; use a TranslateTransform.");
                break;
            default:
                throw new ArgumentException($"SpringEasing.SetScalar: unsupported property '{propertyName}'");
        }
    }

    /// <summary>Response/damping pair, so call sites can pass a named spring instead of two numbers.</summary>
    public readonly record struct Tokens(double Response, double DampingFraction)
    {
        /// <summary>Standard spring (response 0.40, damping 0.80) — most card transitions.</summary>
        public static Tokens Standard { get; } = new(0.40, 0.80);

        /// <summary>Tight spring (response 0.35, damping 0.78) — tile entrances, person cards.</summary>
        public static Tokens Tight { get; } = new(0.35, 0.78);
    }
}
