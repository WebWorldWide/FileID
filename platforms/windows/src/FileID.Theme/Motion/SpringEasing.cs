// SpringEasing — a single helper that wraps WinUI 3's
// SpringScalarNaturalMotionAnimation so call sites read like SwiftUI:
//
//     // SwiftUI:  .animation(.spring(response: 0.4, dampingFraction: 0.8), value: x)
//     // WinUI:    SpringEasing.Animate(target, "Translation.Y", final: 0.0,
//     //                                response: 0.4, dampingFraction: 0.8);
//
// The Composition API computes the spring physics on the GPU; visual
// fidelity is essentially identical to SwiftUI's spring system. There's no
// math port — Microsoft.UI.Composition handles it.
//
// Mapping (SwiftUI → Composition):
//   response             → Period (TimeSpan.FromSeconds(response))
//   dampingFraction      → DampingRatio
//
// On reduced-motion, callers should skip the animation entirely and snap
// the property to its final value. The ReducedMotion bridge (Phase 1.6)
// exposes an IObservable<bool> that animation orchestrators consume.

using Microsoft.UI.Composition;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Hosting;

namespace FileID.Theme.Motion;

public static class SpringEasing
{
    /// <summary>
    /// Animate a scalar Composition property (e.g. Translation.X, Opacity,
    /// Scale.X) toward <paramref name="final"/> using a spring with the
    /// given <paramref name="response"/> and <paramref name="dampingFraction"/>.
    ///
    /// The element's existing visual offset is treated as the start;
    /// natural-motion animations on Composition don't take a "from".
    ///
    /// If <paramref name="initialVelocity"/> is supplied, the spring starts
    /// with that velocity (matches SwiftUI's `.spring(...).withVelocity(_:)`).
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

    /// <summary>
    /// Animate the element's Translation (the post-layout offset) by the
    /// given delta over a spring. Equivalent of SwiftUI's
    /// `.offset(x:y:).animation(.spring(...))`.
    /// </summary>
    public static void AnimateTranslation(
        UIElement element,
        float deltaX,
        float deltaY,
        double response,
        double dampingFraction)
    {
        // Make sure this element has a Translation property (off-by-default
        // on UIElements without explicit opt-in).
        ElementCompositionPreview.SetIsTranslationEnabled(element, true);
        AnimateScalar(element, "Translation.X", deltaX, response, dampingFraction);
        AnimateScalar(element, "Translation.Y", deltaY, response, dampingFraction);
    }

    /// <summary>
    /// Animate uniform scale around the element's center.
    /// Equivalent of SwiftUI's `.scaleEffect(s).animation(.spring(...))`.
    /// </summary>
    public static void AnimateScale(
        UIElement element,
        float finalScale,
        double response,
        double dampingFraction)
    {
        var visual = ElementCompositionPreview.GetElementVisual(element);
        var size = visual.Size;
        // Anchor scale around the visual's center so growth/shrink is symmetric.
        visual.CenterPoint = new System.Numerics.Vector3(size.X / 2, size.Y / 2, 0);
        AnimateScalar(element, "Scale.X", finalScale, response, dampingFraction);
        AnimateScalar(element, "Scale.Y", finalScale, response, dampingFraction);
    }

    /// <summary>
    /// Animate opacity from current to <paramref name="final"/>.
    /// </summary>
    public static void AnimateOpacity(
        UIElement element,
        float final,
        double response,
        double dampingFraction)
        => AnimateScalar(element, "Opacity", final, response, dampingFraction);

    /// <summary>
    /// Snap a scalar property without animation. Use when reduced-motion
    /// is on, or when you want to set the final state immediately.
    /// </summary>
    public static void SetScalar(UIElement element, string propertyName, float value)
    {
        var visual = ElementCompositionPreview.GetElementVisual(element);
        // StopAnimation clears any in-flight spring on this property; then
        // we set the property directly via an ExpressionAnimation snapshot.
        visual.StopAnimation(propertyName);
        // Translation is exposed via Visual.Properties (not directly on Visual);
        // for the basics we cover here, set via the property API.
        switch (propertyName)
        {
            case "Opacity":      visual.Opacity = value; break;
            case "Scale.X":      visual.Scale = new System.Numerics.Vector3(value, visual.Scale.Y, visual.Scale.Z); break;
            case "Scale.Y":      visual.Scale = new System.Numerics.Vector3(visual.Scale.X, value, visual.Scale.Z); break;
            case "Translation.X":
            case "Translation.Y":
                // Translation isn't a top-level Visual property; consumers
                // that need a hard snap should set the parent's
                // RenderTransform or use a CompositionPropertySet. Phase 1.6
                // wires the small set of consumers that need this; for now
                // it's an explicit no-op + a debug warning.
                System.Diagnostics.Debug.WriteLine(
                    $"SpringEasing.SetScalar({propertyName}): direct set not supported for Translation; use a TranslateTransform.");
                break;
            default:
                throw new ArgumentException($"SpringEasing.SetScalar: unsupported property '{propertyName}'");
        }
    }

    /// <summary>
    /// Token bundle. Use this when you want to write callsites like
    /// `SpringEasing.Standard` rather than carrying response/damping pairs around.
    /// </summary>
    public readonly record struct Tokens(double Response, double DampingFraction)
    {
        /// <summary>Standard spring (response 0.40, damping 0.80) — most card transitions.</summary>
        public static Tokens Standard { get; } = new(0.40, 0.80);

        /// <summary>Tight spring (response 0.35, damping 0.78) — tile entrances, person cards.</summary>
        public static Tokens Tight { get; } = new(0.35, 0.78);
    }
}
