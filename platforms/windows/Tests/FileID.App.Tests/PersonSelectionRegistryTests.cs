using FileID.Services;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class PersonSelectionRegistryTests
{
    [Fact]
    public void OpenPersonWinsOverCheckedPeople()
    {
        var registry = new PersonSelectionRegistry();
        registry.SetChecked(new long[] { 9, 3 });
        registry.OpenPerson(7);
        Assert.Equal(7, Assert.Single(registry.Selection));
        registry.ClosePerson(8);
        Assert.Equal(7, Assert.Single(registry.Selection));
        registry.ClosePerson(7);
        Assert.Equal("3,9", string.Join(",", registry.Selection));
        registry.Clear();
        Assert.Empty(registry.Selection);
    }

    [Fact]
    public void CheckedPeopleAreSortedDistinctPositiveAndCappedAtFifty()
    {
        var ids = Enumerable.Range(1, 80).Select(i => (long)(81 - i)).Append(5).Append(0).Append(-2);
        var resolved = PersonSelectionRegistry.Resolve(null, ids);
        Assert.Equal(PersonSelectionRegistry.MaxPeople, resolved.Count);
        Assert.Equal(Enumerable.Range(1, 50).Select(i => (long)i), resolved);
        Assert.Empty(PersonSelectionRegistry.Resolve(null, Array.Empty<long>()));
        Assert.Equal(4, Assert.Single(PersonSelectionRegistry.Resolve(4, ids)));
    }

    [Fact]
    public void ChatContextCarriesPeopleOnlyWhenThereAreAny()
    {
        var people = Enumerable.Range(1, 60).Select(i => (long)i).ToArray();
        var context = ChatActionPolicy.Context(null, null, people);
        Assert.Equal(50, context.PersonIDs!.Count);
        Assert.Equal(1, context.PersonIDs[0]);
        Assert.Null(ChatActionPolicy.Context(null, null, Array.Empty<long>()).PersonIDs);
        Assert.Null(ChatActionPolicy.Context(null, null).PersonIDs);
    }

    [Fact]
    public void PeoplePublishesAndChatReadsTheSelection()
    {
        var people = ReadSource("Views", "People", "PeopleView.xaml.cs");
        Assert.Equal(2, people.Split("PersonSelectionRegistry.Instance.OpenPerson(pc.ClusterId);").Length - 1);
        Assert.Equal(2, people.Split("finally { PersonSelectionRegistry.Instance.ClosePerson(pc.ClusterId); }").Length - 1);
        Assert.Contains("PersonSelectionRegistry.Instance.SetChecked(", people, StringComparison.Ordinal);
        Assert.Contains("PersonSelectionRegistry.Instance.Clear();", people, StringComparison.Ordinal);
        var chat = ReadSource("Views", "Workbenches", "ChatWorkbench.cs");
        Assert.Contains("PersonSelectionRegistry.Instance.Selection", chat, StringComparison.Ordinal);
    }

    private static string ReadSource(params string[] segments)
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
        {
            var root = Path.Combine(directory.FullName, "platforms", "windows", "src", "FileID.App");
            if (Directory.Exists(root)) return File.ReadAllText(Path.Combine([root, .. segments]));
        }
        throw new DirectoryNotFoundException("Windows app sources were not found.");
    }
}
