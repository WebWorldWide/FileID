import Foundation
import CoreFoundation
import Testing
@testable import FileIDShared

@Suite("IPC schema conformance")
struct IPCSchemaConformanceTests {
    @Test("Every schema payload decodes and re-encodes without structural drift")
    func everyPayloadMatchesCanonicalSchema() throws {
        let schema = try Self.loadSchema()
        let definitions = try #require(schema["$defs"] as? [String: Any])
        try checkUnion(
            name: "CommandPayload",
            envelopeName: "IPCCommand",
            definitions: definitions,
            envelope: { payload in ["id": "schema-test", "payload": payload] },
            roundTrip: { data in
                let value = try IPCCoder.decoder.decode(IPCCommand.self, from: data)
                return try IPCCoder.encoder.encode(value)
            }
        )
        try checkUnion(
            name: "EventPayload",
            envelopeName: "IPCEvent",
            definitions: definitions,
            envelope: { payload in ["t": "2026-01-01T00:00:00Z", "payload": payload] },
            roundTrip: { data in
                let value = try IPCCoder.decoder.decode(IPCEvent.self, from: data)
                return try IPCCoder.encoder.encode(value)
            }
        )
    }

    @Test("Validator enforces every constraint keyword used by the canonical schema")
    func validatorEnforcesValueConstraints() {
        let definitions: [String: Any] = [:]
        #expect(throws: SchemaError.self) {
            try validate("wrong", schema: ["type": "string", "pattern": "^allowed$"], definitions: definitions, path: "pattern")
        }
        #expect(throws: SchemaError.self) {
            try validate("not-a-date", schema: ["type": "string", "format": "date-time"], definitions: definitions, path: "format")
        }
        #expect(throws: SchemaError.self) {
            try validate(-1, schema: ["type": "integer", "minimum": 0], definitions: definitions, path: "minimum")
        }
        #expect(throws: SchemaError.self) {
            try validate(2.0, schema: ["type": "number", "maximum": 1.0], definitions: definitions, path: "maximum")
        }
        #expect(throws: SchemaError.self) {
            try validate([1], schema: ["type": "array", "minItems": 2], definitions: definitions, path: "minItems")
        }
        #expect(throws: SchemaError.self) {
            try validate([1, 2], schema: ["type": "array", "maxItems": 1], definitions: definitions, path: "maxItems")
        }
    }

    private func checkUnion(
        name: String,
        envelopeName: String,
        definitions: [String: Any],
        envelope: ([String: Any]) -> [String: Any],
        roundTrip: (Data) throws -> Data
    ) throws {
        let union = try #require(definitions[name] as? [String: Any])
        let variants = try #require(union["oneOf"] as? [[String: Any]])
        let envelopeSchema = try #require(definitions[envelopeName] as? [String: Any])
        var checked = Set<String>()

        for variant in variants {
            let payload = try #require(synthesize(variant, definitions: definitions) as? [String: Any])
            let tag = try #require(payload.keys.first)
            do {
                let inputObject = envelope(payload)
                let input = try JSONSerialization.data(withJSONObject: inputObject)
                let output = try roundTrip(input)
                let object = try JSONSerialization.jsonObject(with: output)
                try validate(object, schema: envelopeSchema, definitions: definitions, path: envelopeName)
                try requireNonNullFieldsPreserved(inputObject, output: object, path: envelopeName)
                checked.insert(tag)
            } catch {
                Issue.record("\(envelopeName).\(tag) drifted from shared/ipc-schema/ipc.schema.json: \(error)")
            }
        }

        let expected = Set(variants.compactMap { ($0["required"] as? [String])?.first })
        #expect(checked == expected)
    }

    private func synthesize(_ schema: [String: Any], definitions: [String: Any]) throws -> Any {
        if let reference = schema["$ref"] as? String {
            return try synthesize(try resolve(reference, definitions: definitions), definitions: definitions)
        }
        if let constant = schema["const"] {
            return constant
        }
        if let choices = (schema["oneOf"] as? [[String: Any]])
                ?? (schema["anyOf"] as? [[String: Any]]),
           let selected = choices.first(where: { preferredType($0["type"]) != "null" })
                ?? choices.first {
            return try synthesize(selected, definitions: definitions)
        }
        if let values = schema["enum"] as? [Any], let first = values.first {
            return first
        }

        let type = preferredType(schema["type"])
        switch type {
        case "object":
            let properties = schema["properties"] as? [String: [String: Any]] ?? [:]
            return try Dictionary(uniqueKeysWithValues: properties.keys.sorted().map { key in
                let property = try #require(properties[key])
                return (key, try synthesize(property, definitions: definitions))
            })
        case "array":
            guard let items = schema["items"] as? [String: Any] else { return [] }
            let count = max(1, schema["minItems"] as? Int ?? 0)
            return try (0..<count).map { _ in
                try synthesize(items, definitions: definitions)
            }
        case "string":
            if schema["format"] as? String == "date-time" {
                return "2026-01-01T00:00:00Z"
            }
            if let pattern = schema["pattern"] as? String {
                for candidate in ["applyTags", "renameFiles", "trashFiles"]
                where try string(candidate, matches: pattern) {
                    return candidate
                }
                throw SchemaError("cannot synthesize value for pattern \(pattern)")
            }
            return "test"
        case "integer":
            return Int(numericConstraint(schema, "minimum") ?? 1)
        case "number":
            return numericConstraint(schema, "minimum") ?? 1.0
        case "boolean":
            return false
        case "null":
            return NSNull()
        default:
            throw SchemaError("cannot synthesize schema \(schema)")
        }
    }

    private func validate(
        _ value: Any,
        schema: [String: Any],
        definitions: [String: Any],
        path: String
    ) throws {
        if let reference = schema["$ref"] as? String {
            try validate(value, schema: resolve(reference, definitions: definitions), definitions: definitions, path: path)
            return
        }
        if let choices = schema["oneOf"] as? [[String: Any]] {
            let matches = choices.filter { choice in
                (try? validate(value, schema: choice, definitions: definitions, path: path)) != nil
            }
            guard matches.count == 1 else {
                throw SchemaError("\(path) matched \(matches.count) oneOf branches")
            }
            return
        }
        if let choices = schema["anyOf"] as? [[String: Any]] {
            guard choices.contains(where: { choice in
                (try? validate(value, schema: choice, definitions: definitions, path: path)) != nil
            }) else {
                throw SchemaError("\(path) matched no anyOf branch")
            }
            return
        }
        if let constant = schema["const"], !jsonEqual(value, constant) {
            throw SchemaError("\(path) did not equal its const")
        }
        if let values = schema["enum"] as? [Any], !values.contains(where: { jsonEqual(value, $0) }) {
            throw SchemaError("\(path) was outside its enum")
        }

        let allowedTypes = typeNames(schema["type"])
        let actualType = jsonType(value)
        let typeMatches = allowedTypes.contains(actualType)
            || (actualType == "integer" && allowedTypes.contains("number"))
        if !allowedTypes.isEmpty && !typeMatches {
            throw SchemaError("\(path) had type \(actualType); expected \(allowedTypes)")
        }

        if let text = value as? String {
            if let pattern = schema["pattern"] as? String,
               try !string(text, matches: pattern) {
                throw SchemaError("\(path) did not match pattern \(pattern)")
            }
            if schema["format"] as? String == "date-time",
               ISO8601DateFormatter().date(from: text) == nil {
                throw SchemaError("\(path) was not an ISO 8601 date-time")
            }
        }
        if let number = value as? NSNumber, actualType != "boolean" {
            let numeric = number.doubleValue
            if let minimum = numericConstraint(schema, "minimum"), numeric < minimum {
                throw SchemaError("\(path) was below minimum \(minimum)")
            }
            if let maximum = numericConstraint(schema, "maximum"), numeric > maximum {
                throw SchemaError("\(path) exceeded maximum \(maximum)")
            }
        }

        if let object = value as? [String: Any] {
            let properties = schema["properties"] as? [String: [String: Any]] ?? [:]
            let required = schema["required"] as? [String] ?? []
            for key in required where object[key] == nil {
                throw SchemaError("\(path) omitted required key \(key)")
            }
            if schema["additionalProperties"] as? Bool == false {
                let extras = Set(object.keys).subtracting(properties.keys)
                if !extras.isEmpty {
                    throw SchemaError("\(path) emitted undeclared keys \(extras.sorted())")
                }
            }
            for (key, child) in object {
                if let childSchema = properties[key] {
                    try validate(child, schema: childSchema, definitions: definitions, path: "\(path).\(key)")
                }
            }
        } else if let array = value as? [Any] {
            if let minimum = schema["minItems"] as? Int, array.count < minimum {
                throw SchemaError("\(path) had fewer than \(minimum) items")
            }
            if let maximum = schema["maxItems"] as? Int, array.count > maximum {
                throw SchemaError("\(path) had more than \(maximum) items")
            }
            if let itemSchema = schema["items"] as? [String: Any] {
                for (index, child) in array.enumerated() {
                    try validate(child, schema: itemSchema, definitions: definitions, path: "\(path)[\(index)]")
                }
            }
        }
    }

    private func requireNonNullFieldsPreserved(_ input: Any, output: Any, path: String) throws {
        if let inputObject = input as? [String: Any] {
            guard let outputObject = output as? [String: Any] else {
                throw SchemaError("\(path) changed an object into \(jsonType(output))")
            }
            for (key, inputValue) in inputObject where !(inputValue is NSNull) {
                guard let outputValue = outputObject[key] else {
                    throw SchemaError("\(path) dropped populated schema field \(key)")
                }
                try requireNonNullFieldsPreserved(
                    inputValue, output: outputValue, path: "\(path).\(key)")
            }
        } else if let inputArray = input as? [Any] {
            guard let outputArray = output as? [Any], outputArray.count == inputArray.count else {
                throw SchemaError("\(path) changed populated array length")
            }
            for (index, pair) in zip(inputArray, outputArray).enumerated() {
                try requireNonNullFieldsPreserved(
                    pair.0, output: pair.1, path: "\(path)[\(index)]")
            }
        }
    }

    private func numericConstraint(_ schema: [String: Any], _ key: String) -> Double? {
        (schema[key] as? NSNumber)?.doubleValue
    }

    private func string(_ value: String, matches pattern: String) throws -> Bool {
        let expression = try NSRegularExpression(pattern: pattern)
        let range = NSRange(value.startIndex..<value.endIndex, in: value)
        return expression.firstMatch(in: value, range: range) != nil
    }

    private func resolve(_ reference: String, definitions: [String: Any]) throws -> [String: Any] {
        let prefix = "#/$defs/"
        guard reference.hasPrefix(prefix) else {
            throw SchemaError("unsupported reference \(reference)")
        }
        let components = reference.dropFirst(prefix.count).split(separator: "/").map(String.init)
        guard let first = components.first, var value = definitions[first] else {
            throw SchemaError("unresolved reference \(reference)")
        }
        for component in components.dropFirst() {
            guard let object = value as? [String: Any], let next = object[component] else {
                throw SchemaError("unresolved reference \(reference)")
            }
            value = next
        }
        guard let schema = value as? [String: Any] else {
            throw SchemaError("reference isn't a schema object: \(reference)")
        }
        return schema
    }

    private func preferredType(_ raw: Any?) -> String? {
        typeNames(raw).first(where: { $0 != "null" }) ?? typeNames(raw).first
    }

    private func typeNames(_ raw: Any?) -> [String] {
        if let value = raw as? String { return [value] }
        return raw as? [String] ?? []
    }

    private func jsonType(_ value: Any) -> String {
        if value is NSNull { return "null" }
        if value is [String: Any] { return "object" }
        if value is [Any] { return "array" }
        if value is String { return "string" }
        if let number = value as? NSNumber {
            if CFGetTypeID(number) == CFBooleanGetTypeID() { return "boolean" }
            let encoding = String(cString: number.objCType)
            return encoding == "f" || encoding == "d" ? "number" : "integer"
        }
        return "unknown"
    }

    private func jsonEqual(_ lhs: Any, _ rhs: Any) -> Bool {
        guard JSONSerialization.isValidJSONObject([lhs]), JSONSerialization.isValidJSONObject([rhs]),
              let left = try? JSONSerialization.data(withJSONObject: [lhs], options: [.sortedKeys]),
              let right = try? JSONSerialization.data(withJSONObject: [rhs], options: [.sortedKeys])
        else { return false }
        return left == right
    }

    private static func loadSchema() throws -> [String: Any] {
        var root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        while root.path != "/" {
            let candidate = root.appendingPathComponent("shared/ipc-schema/ipc.schema.json")
            if FileManager.default.fileExists(atPath: candidate.path) {
                let data = try Data(contentsOf: candidate)
                return try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
            }
            root.deleteLastPathComponent()
        }
        throw SchemaError("could not locate shared/ipc-schema/ipc.schema.json")
    }

    private struct SchemaError: Error, CustomStringConvertible {
        let description: String
        init(_ description: String) { self.description = description }
    }
}
