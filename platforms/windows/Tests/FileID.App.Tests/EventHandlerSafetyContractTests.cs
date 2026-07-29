using System.Text.RegularExpressions;
using Xunit;

namespace FileID.App.Tests;

/// <summary>
/// The stowed-exception crash class: an exception that escapes a
/// PropertyChanged handler tears down the WinUI process rather than surfacing
/// as a catchable error. The app's standing invariant is that every such
/// handler body runs inside <c>DebugLog.SafeRun</c>. These tests hold the whole
/// app to it so a newly added subscription can't silently reopen the class.
/// </summary>
public sealed class EventHandlerSafetyContractTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    /// <summary>
    /// Matches <c>receiver.PropertyChanged += HandlerName;</c> — a subscription
    /// to <em>another</em> object's notifications, which is where the crash
    /// class lives. A bare <c>PropertyChanged += handler;</c> (EngineClient
    /// awaiting its own one-shot reply) is deliberately out of scope: those
    /// bodies only compare a property name and call <c>TrySetResult</c>, which
    /// cannot throw.
    /// </summary>
    private static readonly Regex Subscription =
        new(@"[\w\.]+\.PropertyChanged\s*\+=\s*(?<handler>\w+)\s*;", RegexOptions.Compiled);

    [Fact]
    public void EveryPropertyChangedHandlerInTheAppIsSafeRunWrapped()
    {
        var offenders = new List<string>();
        var examined = 0;

        foreach (var file in AppSourceFiles())
        {
            var text = File.ReadAllText(file);
            var lines = text.Split('\n');
            var relative = Path.GetRelativePath(RepoRoot, file);

            foreach (Match match in Subscription.Matches(text))
            {
                var handler = match.Groups["handler"].Value;
                examined++;

                // `-=` unsubscriptions are matched by the same shape only when
                // written as `+=`, so every hit here is a real subscription.
                var bodyStart = FindHandlerBody(lines, handler);
                if (bodyStart < 0)
                {
                    offenders.Add($"{relative}: handler '{handler}' has no resolvable definition");
                    continue;
                }

                var window = string.Join('\n', lines.Skip(bodyStart).Take(60));
                if (!window.Contains("SafeRun", StringComparison.Ordinal))
                {
                    offenders.Add($"{relative}: handler '{handler}' body is not SafeRun-wrapped");
                }
            }
        }

        // Guards against the regex silently matching nothing and the assertion
        // below passing vacuously after a refactor renames the event.
        Assert.True(examined >= 30, $"Expected to inspect the app's PropertyChanged subscriptions; saw {examined}.");

        Assert.True(
            offenders.Count == 0,
            "Every PropertyChanged handler must wrap its body in DebugLog.SafeRun so a throw "
                + "cannot escape as a stowed exception:\n  " + string.Join("\n  ", offenders));
    }

    /// <summary>
    /// ReducedMotion raises PropertyChanged from a threadpool thread (the WinRT
    /// UISettings callback). A bare multicast invoke there is a process kill —
    /// one throwing subscriber escapes unhandled AND starves every later
    /// subscriber. It must fan out per-subscriber with isolation instead.
    /// </summary>
    [Fact]
    public void ReducedMotionIsolatesEachSubscriberFromItsSiblings()
    {
        var source = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.Theme", "Motion", "ReducedMotion.cs"));

        Assert.Contains("GetInvocationList()", source, StringComparison.Ordinal);
        Assert.DoesNotContain("PropertyChanged?.Invoke(", source, StringComparison.Ordinal);

        var fanOut = source.IndexOf("GetInvocationList()", StringComparison.Ordinal);
        var guard = source.IndexOf("catch", fanOut, StringComparison.Ordinal);
        Assert.True(guard > fanOut, "Each subscriber invocation must be individually guarded.");
    }

    /// <summary>Index of the first line of <paramref name="handler"/>'s body,
    /// whether it is declared as a method or assigned as a lambda.</summary>
    private static int FindHandlerBody(string[] lines, string handler)
    {
        var method = new Regex(@"\b(?:void|Task)\s+" + Regex.Escape(handler) + @"\s*\(");
        var lambda = new Regex(@"\b" + Regex.Escape(handler) + @"\s*=\s*(?:\(|async\s*\()");

        for (var i = 0; i < lines.Length; i++)
        {
            if (method.IsMatch(lines[i]) || lambda.IsMatch(lines[i]))
            {
                return i;
            }
        }
        return -1;
    }

    private static IEnumerable<string> AppSourceFiles()
        => Directory
            .EnumerateFiles(PathInRepo("platforms", "windows", "src", "FileID.App"), "*.cs", SearchOption.AllDirectories)
            .Where(p => !p.Contains($"{Path.DirectorySeparatorChar}bin{Path.DirectorySeparatorChar}", StringComparison.Ordinal)
                     && !p.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.Ordinal));

    private static string PathInRepo(params string[] parts)
        => Path.Combine([RepoRoot, .. parts]);

    private static string FindRepoRoot()
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
        {
            if (File.Exists(Path.Combine(directory.FullName, "AGENTS.md"))
                && Directory.Exists(Path.Combine(directory.FullName, "platforms", "windows")))
            {
                return directory.FullName;
            }
        }
        throw new DirectoryNotFoundException("Could not find the FileID repository root from the test output directory.");
    }
}
