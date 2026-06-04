// JsonSerializerOptions configured for IPC. Centralized so app and engine
// always agree on date format, naming policy, and number handling.
//
// Mirror of Swift IPCCoder (platforms/apple/shared/Sources/FileIDShared/IPCCoder.swift).
//
// Notable choices:
//   * camelCase property naming policy (matches Swift Codable default).
//   * ISO8601 date format (Swift uses ISO8601 too via JSONEncoder.dateEncodingStrategy).
//   * Strict JSON: refuses comments, trailing commas, single-quoted strings.
//     We do not deserialize untrusted input from anywhere except our own
//     engine, but defense-in-depth is cheap.
//   * No indent: each line is one frame. Indent would put newlines INSIDE
//     a frame, breaking the wire convention.

using System.IO;
using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace FileID.IpcSchema;

public static class IpcCoder
{
    /// <summary>
    /// Default JsonSerializerOptions for IPC. Reuse this — never construct
    /// fresh options per call (System.Text.Json caches metadata against the
    /// options object identity, and ad-hoc options trigger reflection
    /// warm-up costs on every encode).
    /// </summary>
    public static readonly JsonSerializerOptions Options = BuildOptions();

    private static JsonSerializerOptions BuildOptions()
    {
        var o = new JsonSerializerOptions
        {
            PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
            // We don't case-insensitive-match keys: Swift produces exactly the
            // case we expect; mismatches mean schema drift, surface them.
            PropertyNameCaseInsensitive = false,
            // Strict input.
            ReadCommentHandling = JsonCommentHandling.Disallow,
            AllowTrailingCommas = false,
            // Stable wire format: don't write indented (frames are 1-line).
            WriteIndented = false,
            // Do NOT escape non-ASCII; matches Swift JSONEncoder default and
            // saves bytes for any UTF-8 paths/captions.
            Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
            // Emit nulls explicitly when present; Swift Codable encodes
            // optional fields as `null` by default, not omitted.
            DefaultIgnoreCondition = JsonIgnoreCondition.Never,
            NumberHandling = JsonNumberHandling.AllowNamedFloatingPointLiterals,
        };

        // Register the externally-tagged-union converters explicitly. The
        // [JsonConverter] attribute on the abstract record types isn't
        // reliably picked up by System.Text.Json — the attribute path goes
        // through ObjectDefaultConverter which fails to instantiate the
        // abstract type. Adding to Options.Converters takes priority.
        o.Converters.Add(new CommandPayloadJsonConverter());
        o.Converters.Add(new EventPayloadJsonConverter());

        // Enum naming: force camelCase on enum string output to match the
        // schema (Swift Codable default). The attribute-based
        // JsonStringEnumConverter<T> defaults to PascalCase; only the
        // programmatic constructor honors a naming policy.
        o.Converters.Add(new JsonStringEnumConverter<ScanPhase>(JsonNamingPolicy.CamelCase));
        o.Converters.Add(new JsonStringEnumConverter<JobCategory>(JsonNamingPolicy.CamelCase));
        o.Converters.Add(new JsonStringEnumConverter<LogLevel>(JsonNamingPolicy.CamelCase));
        o.Converters.Add(new JsonStringEnumConverter<DeepAnalyzeStartingPhase>(JsonNamingPolicy.CamelCase));

        return o;
    }

    /// <summary>
    /// Serialize a value as a single line of JSON, terminated by '\n'. Caller
    /// writes the resulting bytes to the wire (stdin / pipe / socket).
    /// Mirrors Swift IPCCoder.encodeLine.
    /// </summary>
    public static byte[] EncodeLine<T>(T value)
    {
        using var stream = new MemoryStream();
        using (var writer = new Utf8JsonWriter(stream, new JsonWriterOptions
        {
            Indented = false,
            Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
        }))
        {
            JsonSerializer.Serialize(writer, value, Options);
        }
        stream.WriteByte((byte)'\n');
        return stream.ToArray();
    }

    /// <summary>
    /// Decode a single frame (with or without the trailing newline).
    /// </summary>
    public static T Decode<T>(string frame)
    {
        // Span overload (net8.0) — no intermediate string copy. TrimEnd on the
        // span preserves the trailing-newline tolerance; behavior is identical.
        return JsonSerializer.Deserialize<T>(frame.AsSpan().TrimEnd('\n'), Options)
            ?? throw new JsonException($"IpcCoder.Decode<{typeof(T).Name}>: deserializer returned null");
    }

    /// <summary>
    /// Encode without the trailing newline. Useful for tests + logging.
    /// </summary>
    public static string Encode<T>(T value) =>
        Encoding.UTF8.GetString(EncodeLine(value)).TrimEnd('\n');
}
