// Schema-conformance suite — C# twin of the Rust engine's variant coverage
// tests, checked against the canonical contract itself. For every
// CommandPayload / EventPayload variant we keep an exemplar instance,
// serialize it through IpcCoder (the exact wire path), and assert against
// shared/ipc-schema/ipc.schema.json (copied beside the test binary):
//   1. the payload object carries exactly one variant tag,
//   2. serialized keys are a subset of the schema variant's properties,
//      recursively wherever the schema declares additionalProperties:false,
//   3. the schema variant's required keys all appear in the serialized form,
//   4. the C# tag set equals the schema's oneOf tag set in BOTH directions,
//      modulo the documented platform-divergence allowlists below.
//
// When you add a payload variant, the reflection tests fail until you add
// an exemplar here — that is the point.

using System.Text.Json;
using Xunit;

namespace FileID.IpcSchema.Tests;

public class SchemaConformanceTests
{
    private static readonly JsonDocument _schema = JsonDocument.Parse(
        File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "ipc.schema.json")));

    // Platform-divergence allowlists for the two-way tag comparison. A tag
    // belongs here only when the schema deliberately carries a variant for
    // one platform (or a platform carries one the schema doesn't), and each
    // entry needs a matching note in the schema description. The known
    // historical divergence is macOS's startScan(rootBookmark:) shape — the
    // schema standardized on rootPath and the Swift app now resolves the
    // security-scoped bookmark to a path before sending (IpcCommandTests
    // pins the no-rootBookmark wire bytes), so every variant is currently
    // shared and all four lists are empty.
    private static readonly IReadOnlySet<string> _schemaOnlyCommandTags = new HashSet<string>(StringComparer.Ordinal);
    private static readonly IReadOnlySet<string> _csharpOnlyCommandTags = new HashSet<string>(StringComparer.Ordinal);
    private static readonly IReadOnlySet<string> _schemaOnlyEventTags = new HashSet<string>(StringComparer.Ordinal);
    private static readonly IReadOnlySet<string> _csharpOnlyEventTags = new HashSet<string>(StringComparer.Ordinal);

    // Hoisted constant arrays (CA1861) shared by the exemplars.
    private static readonly long[] _exampleFileIds = { 1, 2, 3 };
    private static readonly string[] _exampleTags = { "hawaii", "sunset" };
    private static readonly long[] _examplePersonIds = { 1, 2 };
    private static readonly long[] _exampleFaceIds = { 10, 11, 12 };
    private static readonly string[] _exampleSkippedStages = { "ocr" };
    private static readonly float[] _embedding512 = new float[512];

    [Fact]
    public void CommandExemplars_CoverEveryVariantType()
    {
        AssertExemplarsCoverUnion(typeof(CommandPayload), CommandExemplars().Select(p => p.GetType()));
    }

    [Fact]
    public void EventExemplars_CoverEveryVariantType()
    {
        AssertExemplarsCoverUnion(typeof(EventPayload), EventExemplars().Select(p => p.GetType()));
    }

    [Fact]
    public void EveryCommandVariant_WireKeysConformToSchema()
    {
        var variants = SchemaVariants("CommandPayload");
        var errors = new List<string>();
        foreach (var payload in CommandExemplars())
        {
            CheckTaggedPayload("IPCCommand", variants, IpcCoder.Encode(new IpcCommand("conformance", payload)), payload.GetType().Name, errors);
        }
        AssertNoErrors(errors);
    }

    [Fact]
    public void EveryEventVariant_WireKeysConformToSchema()
    {
        var variants = SchemaVariants("EventPayload");
        var errors = new List<string>();
        foreach (var payload in EventExemplars())
        {
            CheckTaggedPayload("IPCEvent", variants, IpcCoder.Encode(IpcEvent.Now(payload)), payload.GetType().Name, errors);
        }
        AssertNoErrors(errors);
    }

    [Fact]
    public void CommandTagSet_MatchesSchemaBothWays()
    {
        AssertTagSetsEqual(
            SchemaVariants("CommandPayload").Keys,
            CommandExemplars().Select(p => WireTag(IpcCoder.Encode(new IpcCommand("conformance", p)))),
            _schemaOnlyCommandTags,
            _csharpOnlyCommandTags,
            "CommandPayload");
    }

    [Fact]
    public void EventTagSet_MatchesSchemaBothWays()
    {
        AssertTagSetsEqual(
            SchemaVariants("EventPayload").Keys,
            EventExemplars().Select(p => WireTag(IpcCoder.Encode(IpcEvent.Now(p)))),
            _schemaOnlyEventTags,
            _csharpOnlyEventTags,
            "EventPayload");
    }

    // Negative self-tests, twinned with the Rust suite's
    // checker_rejects_wrong_cased_key / checker_rejects_missing_required_key:
    // the checker must reject the exact L1 drift class (fileId vs fileID)
    // and a missing schema-required key, or this suite guards nothing.

    [Fact]
    public void Checker_RejectsWrongCasedKey()
    {
        var variants = SchemaVariants("CommandPayload");
        var errors = new List<string>();
        using var doc = JsonDocument.Parse("""{"fileId":42,"modelKind":"m"}""");
        CheckValue(variants["deepAnalyzeFile"], doc.RootElement, "deepAnalyzeFile", errors);
        Assert.Contains(errors, e => e.Contains("'fileId'", StringComparison.Ordinal));
    }

    [Fact]
    public void Checker_RejectsMissingRequiredKey()
    {
        var variants = SchemaVariants("CommandPayload");
        var errors = new List<string>();
        using var doc = JsonDocument.Parse("""{"fileID":42}""");
        CheckValue(variants["deepAnalyzeFile"], doc.RootElement, "deepAnalyzeFile", errors);
        Assert.Contains(errors, e => e.Contains("'modelKind'", StringComparison.Ordinal));
    }

    // ── Exemplars ────────────────────────────────────────────────────────
    // One fully-populated instance per variant, constructed exactly as the
    // app constructs them. Optional fields are set so the serialized keys
    // exercise the variant's full schema property surface.

    private static IReadOnlyList<CommandPayload> CommandExemplars() => new CommandPayload[]
    {
        new StartScanCommand(@"C:\Users\adam\Pictures", "Pictures", Rescan: true),
        new PauseScanCommand(),
        new ResumeScanCommand(),
        new CancelScanCommand(),
        new RequestStatusCommand(),
        new ShutdownCommand(),
        new RunFaceClusteringCommand(),
        new VerifyCudaPackCommand(),
        new DeepAnalyzeFileCommand(42, "qwen2_5_vl_7b"),
        new DeepAnalyzeFolderCommand(@"C:\Users\adam\Pictures\2024", "qwen2_5_vl_7b"),
        new DeepAnalyzeAllCommand("qwen2_5_vl_7b", SkipExisting: true, TagsOnly: true, ProposeRenames: true),
        new DeepAnalyzeCancelCommand(),
        new PrewarmModelCommand("clip_text"),
        new CancelPrewarmCommand("clip_text"),
        new PlanRestructureCommand(@"C:\Users\adam\Pictures"),
        new ApplyRestructureCommand(
            @"C:\Users\adam\Pictures",
            new[] { ExampleMove() },
            UseSymlinks: false),
        new ApplyTagsCommand(_exampleFileIds, _exampleTags, "replace"),
        new RenameFilesCommand(new[] { new RenameEntry(1, "Renamed.jpg") }),
        new TrashFilesCommand(_exampleFileIds),
        new MergeClustersCommand(1, 2),
        new EmbedTextQueryCommand("sunset at the beach", "q-1"),
        new RenamePersonCommand(1, Title: "Dr", FirstName: "Mary", MiddleName: "Q", LastName: "Smith", Suffix: "Jr"),
        new MarkPersonsAsUnknownCommand(_examplePersonIds),
        new FindMergeSuggestionsCommand(),
        new MarkPersonsDifferentCommand(1, 2, 10, 20),
        new EmbedImageQueryCommand(1, "q-2"),
        new GenerateVideoThumbnailCommand(@"C:\Users\adam\Videos\clip.mp4", 1_700_000_000.0),
        new RestoreFromTrashCommand("00000000-0000-0000-0000-000000000000"),
        new RevertMergeCommand(1, 2, _exampleFaceIds),
        new WipeLibraryCommand(),
    };

    private static IReadOnlyList<EventPayload> EventExemplars() => new EventPayload[]
    {
        new ReadyEvent(new EngineInfo("1.0.0", 1234, 14, 16.0, ExampleHardware())),
        new ProgressEvent(new ScanProgress("sess-1", ScanPhase.Tagging, 100, 100, 50, 1, 87.4, 12.5, 612, 4200)),
        new PhaseChangedEvent(ScanPhase.PostScan),
        new DiscoveryCompleteEvent(50_000),
        new FileDoneEventWrapper(new FileDoneEvent(@"C:\Users\adam\Pictures\IMG_0001.jpg", "image", 12.5, false, null, _exampleSkippedStages)),
        new BatchSummaryEvent(new BatchSummary(1, 64, 128, 2.5, 25.6, 0.9, 10.0, 20.0, 5.0, 9.0, 1.0, 2.0, 612, 4200)),
        new ScanCompleteEvent(new ScanComplete("sess-1", 100, 99, 1, 60.0)),
        new ErrorEvent(new EngineError("model_download_failed", "download timed out", @"C:\Users\adam\AppData\Local\FileID\Models\m.onnx", "mobileclip_s2")),
        new LogEvent(new LogLine(LogLevel.Info, "hello")),
        new FaceClusteringCompleteEvent(new FaceClusteringResult(3, 120, 5, 9.5)),
        new DeepAnalyzeStartingEvent(new DeepAnalyzeStarting("qwen2_5_vl_7b", DeepAnalyzeStartingPhase.LoadingModel, "loading weights")),
        new DeepAnalyzeProgressEvent(new DeepAnalyzeProgress(3, 10, 42.5, @"C:\Users\adam\Pictures\dog.jpg", "qwen2_5_vl_7b", "A dog sits on")),
        new DeepAnalyzeFileDoneEvent(new DeepAnalyzeFileDone(99, "a cat on a couch", "cat_couch.jpg", "qwen2_5_vl_7b")),
        new DeepAnalyzeCompleteEvent(new DeepAnalyzeComplete(10, 0, 60.0, "qwen2_5_vl_7b", false)),
        new ModelDownloadProgressEvent(new ModelDownloadProgress("qwen2_5_vl_7b", 0.5, "downloading", 100, 200)),
        new QueueStateEvent(new QueueState(
            new QueuedJob("job-1", JobCategory.Scan, "Scan Library", 120.0),
            new[] { new QueuedJob("job-2", JobCategory.DeepAnalyze, "Captions", 600.0) },
            720.0)),
        new RestructurePlanEvent(new RestructurePlan(
            @"C:\Users\adam\Pictures",
            new[] { ExampleMove() },
            new[] { new RestructureCategoryCount("Photos/2024", 1) },
            new FolderClassificationCounts(3, 2, 1))),
        new RestructureApplyResultEvent(new RestructureApplyResult(5, 1, "Developer Mode required for symlinks")),
        new BulkActionResultEvent(new BulkActionResult(
            "trashFiles:00000000-0000-0000-0000-000000000000", 2, 1,
            new[] { new BulkActionItem(1, true, null), new BulkActionItem(2, false, "file locked") })),
        new ClipTextEmbeddingEvent(new ClipTextEmbedding("q-1", "sunset at the beach", _embedding512)),
        new MergeSuggestionsEvent(new MergeSuggestions(new[] { new MergeSuggestion(1, 2, 0.93f, 10, 20, 4, 7) })),
        new HardwareReprobedEvent(new HardwareReprobed(ExampleHardware(), "cuDNN missing from PATH")),
        new LibraryWipedEvent(new LibraryWiped(true, "ok")),
        new ThumbnailGeneratedEvent(new ThumbnailGenerated(@"C:\Users\adam\Videos\clip.mp4", 1_700_000_000.0, "AAECAw==")),
    };

    private static RestructureMove ExampleMove() => new(
        1,
        @"C:\Users\adam\Pictures\IMG_0001.jpg",
        @"C:\Users\adam\Pictures\Photos\2024\IMG_0001.jpg",
        "Photos/2024",
        Tier: "Anchor",
        Confidence: "auto",
        Reason: "Photo from 2024");

    private static HardwareInfo ExampleHardware() => new(
        GpuVendor: "nvidia",
        AdapterName: "NVIDIA GeForce RTX 2060",
        ExecutionProvider: "cuda",
        PhysicalCpuCores: 8,
        CudaPackPresent: true,
        OpenvinoPackPresent: false,
        QnnPackPresent: false,
        Recommendation: "");

    // ── Schema plumbing ──────────────────────────────────────────────────

    private static void AssertExemplarsCoverUnion(Type unionBase, IEnumerable<Type> exemplarTypes)
    {
        var concrete = unionBase.Assembly.GetTypes()
            .Where(t => !t.IsAbstract && unionBase.IsAssignableFrom(t))
            .ToHashSet();
        var missing = concrete.Except(exemplarTypes.ToHashSet())
            .Select(t => t.Name)
            .OrderBy(n => n, StringComparer.Ordinal)
            .ToList();
        if (missing.Count > 0)
        {
            Assert.Fail($"{unionBase.Name} variants without a conformance exemplar: {string.Join(", ", missing)}");
        }
    }

    private static void AssertTagSetsEqual(
        IEnumerable<string> schemaTags,
        IEnumerable<string> wireTags,
        IReadOnlySet<string> schemaOnlyAllowlist,
        IReadOnlySet<string> csharpOnlyAllowlist,
        string unionName)
    {
        var schemaSet = schemaTags.ToHashSet(StringComparer.Ordinal);
        var wireSet = wireTags.ToHashSet(StringComparer.Ordinal);

        var missingInCSharp = schemaSet.Except(wireSet).Except(schemaOnlyAllowlist)
            .OrderBy(t => t, StringComparer.Ordinal).ToList();
        var missingInSchema = wireSet.Except(schemaSet).Except(csharpOnlyAllowlist)
            .OrderBy(t => t, StringComparer.Ordinal).ToList();

        if (missingInCSharp.Count > 0)
        {
            Assert.Fail($"{unionName}: schema oneOf variants with no C# twin: {string.Join(", ", missingInCSharp)}");
        }
        if (missingInSchema.Count > 0)
        {
            Assert.Fail($"{unionName}: C# variants missing from the schema oneOf: {string.Join(", ", missingInSchema)}");
        }
    }

    private static void AssertNoErrors(List<string> errors)
    {
        if (errors.Count > 0)
        {
            Assert.Fail(string.Join(Environment.NewLine, errors));
        }
    }

    /// <summary>
    /// Maps each oneOf entry's single tag name to the schema for the tag's
    /// value (the variant body, or its $ref).
    /// </summary>
    private static Dictionary<string, JsonElement> SchemaVariants(string unionName)
    {
        var variants = new Dictionary<string, JsonElement>(StringComparer.Ordinal);
        var oneOf = _schema.RootElement.GetProperty("$defs").GetProperty(unionName).GetProperty("oneOf");
        foreach (var entry in oneOf.EnumerateArray())
        {
            var tag = Assert.Single(entry.GetProperty("properties").EnumerateObject());
            variants.Add(tag.Name, tag.Value);
        }
        return variants;
    }

    private static void CheckTaggedPayload(
        string envelopeDef, Dictionary<string, JsonElement> variants, string json, string typeName, List<string> errors)
    {
        using var doc = JsonDocument.Parse(json);
        CheckValue(_schema.RootElement.GetProperty("$defs").GetProperty(envelopeDef), doc.RootElement, envelopeDef, errors);
        var tag = Assert.Single(doc.RootElement.GetProperty("payload").EnumerateObject());
        if (!variants.TryGetValue(tag.Name, out var bodySchema))
        {
            errors.Add($"{typeName}: wire tag '{tag.Name}' has no schema oneOf variant");
            return;
        }
        CheckValue(bodySchema, tag.Value, tag.Name, errors);
    }

    private static string WireTag(string json)
    {
        using var doc = JsonDocument.Parse(json);
        return Assert.Single(doc.RootElement.GetProperty("payload").EnumerateObject()).Name;
    }

    /// <summary>
    /// Structural subset check: serialized keys must exist in the schema's
    /// properties wherever additionalProperties is false, required keys must
    /// be present, string enums must match, and types must be compatible.
    /// Follows $ref, anyOf, and array items recursively. Deliberately not a
    /// full JSON Schema validator — just the drift classes that have bitten
    /// (renamed keys, missing fields, wrong wrapper shape, enum casing).
    /// </summary>
    private static void CheckValue(JsonElement schema, JsonElement value, string path, List<string> errors)
    {
        schema = Resolve(schema);

        if (schema.TryGetProperty("anyOf", out var anyOf))
        {
            foreach (var alternative in anyOf.EnumerateArray())
            {
                var trial = new List<string>();
                CheckValue(alternative, value, path, trial);
                if (trial.Count == 0)
                {
                    return;
                }
            }
            errors.Add($"{path}: {value.ValueKind} satisfies no anyOf alternative");
            return;
        }

        if (!TypeMatches(schema, value))
        {
            errors.Add($"{path}: serialized {value.ValueKind} does not satisfy schema type {schema.GetProperty("type")}");
            return;
        }

        if (value.ValueKind == JsonValueKind.String && schema.TryGetProperty("enum", out var allowed))
        {
            var s = value.GetString();
            if (!allowed.EnumerateArray().Any(a => a.GetString() == s))
            {
                errors.Add($"{path}: '{s}' is not in the schema enum {allowed}");
            }
        }

        switch (value.ValueKind)
        {
            case JsonValueKind.Object:
                bool closed = schema.TryGetProperty("additionalProperties", out var additional)
                    && additional.ValueKind == JsonValueKind.False;
                bool hasProps = schema.TryGetProperty("properties", out var props);
                foreach (var member in value.EnumerateObject())
                {
                    if (hasProps && props.TryGetProperty(member.Name, out var memberSchema))
                    {
                        CheckValue(memberSchema, member.Value, $"{path}.{member.Name}", errors);
                    }
                    else if (closed)
                    {
                        errors.Add($"{path}: key '{member.Name}' is not in the schema's properties");
                    }
                }
                if (schema.TryGetProperty("required", out var required))
                {
                    foreach (var req in required.EnumerateArray())
                    {
                        if (!value.TryGetProperty(req.GetString()!, out _))
                        {
                            errors.Add($"{path}: required key '{req.GetString()}' missing from serialized form");
                        }
                    }
                }
                break;
            case JsonValueKind.Array:
                if (schema.TryGetProperty("items", out var items))
                {
                    int i = 0;
                    foreach (var element in value.EnumerateArray())
                    {
                        CheckValue(items, element, $"{path}[{i++}]", errors);
                    }
                }
                break;
        }
    }

    private static bool TypeMatches(JsonElement schema, JsonElement value)
    {
        if (!schema.TryGetProperty("type", out var type))
        {
            return true;
        }
        var names = type.ValueKind == JsonValueKind.Array
            ? type.EnumerateArray().Select(t => t.GetString()!).ToArray()
            : new[] { type.GetString()! };
        return names.Any(name => name switch
        {
            "object" => value.ValueKind == JsonValueKind.Object,
            "array" => value.ValueKind == JsonValueKind.Array,
            "string" => value.ValueKind == JsonValueKind.String,
            "boolean" => value.ValueKind is JsonValueKind.True or JsonValueKind.False,
            "null" => value.ValueKind == JsonValueKind.Null,
            "number" or "integer" => value.ValueKind == JsonValueKind.Number,
            _ => false,
        });
    }

    private static JsonElement Resolve(JsonElement schema)
    {
        while (schema.ValueKind == JsonValueKind.Object && schema.TryGetProperty("$ref", out var reference))
        {
            var pointer = reference.GetString()!;
            Assert.StartsWith("#/", pointer);
            var node = _schema.RootElement;
            foreach (var segment in pointer[2..].Split('/'))
            {
                node = node.GetProperty(segment);
            }
            schema = node;
        }
        return schema;
    }
}
