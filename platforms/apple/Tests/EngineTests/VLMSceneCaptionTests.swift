import Testing
import Foundation
@testable import FileIDEngine
@testable import FileIDVLMCore
import FileIDShared

@Suite("VLM scene captions and loop guard")
struct VLMSceneCaptionTests {
    @Test("a block repeated back to back at the tail is found")
    func repeatedTail() {
        let block = Array(100..<130)
        let tokens = Array(0..<20) + block + block
        #expect(VLMGreedyDecoder.repeatedTailLength(tokens) == 30)
    }

    @Test("short or unrepeated text is not a loop")
    func notALoop() {
        #expect(VLMGreedyDecoder.repeatedTailLength(Array(0..<200)) == nil)
        let short = Array(0..<8)
        #expect(VLMGreedyDecoder.repeatedTailLength(short + short) == nil)
        let block = Array(100..<130)
        #expect(VLMGreedyDecoder.repeatedTailLength(block + Array(0..<5) + block) == nil)
    }

    @Test("a looped reply ends at its last full sentence")
    func endsAtSentence() {
        let text = "A form with two fields and a table on the left. The bottom shows a bar"
        #expect(VLMGreedyDecoder.endingAtSentence(text) == "A form with two fields and a table on the left.")
        #expect(VLMGreedyDecoder.endingAtSentence("No sentence end here") == "No sentence end here")
        #expect(VLMGreedyDecoder.endingAtSentence("A. And a long unfinished tail with no stop") == "A. And a long unfinished tail with no stop")
    }

    @Test("the first closed JSON object completes a reply")
    func jsonObject() {
        #expect(!VLMGreedyDecoder.isCompleteJSONObject("{\"moments\":[{\"title\":\"a}\""))
        #expect(!VLMGreedyDecoder.isCompleteJSONObject("```json\n{\"moments\":["))
        #expect(VLMGreedyDecoder.isCompleteJSONObject("{\"moments\":[{\"title\":\"a } \\\" b\"}]}"))
        #expect(VLMGreedyDecoder.isCompleteJSONObject("```json\n{\"moments\":[]}\n```"))
    }

    @Test("the scene prompt asks for a plain caption without filename or tag lines")
    func scenePrompt() {
        #expect(!DeepAnalyze.sceneSystemPrompt.contains("FILENAME"))
        #expect(!DeepAnalyze.sceneSystemPrompt.contains("TAGS"))
        #expect(DeepAnalyze.sceneSystemPrompt.contains("one sampled video frame"))
        #expect(DeepAnalyze.sceneCaptionText("DESCRIPTION: A desk.\n") == "A desk.")
        #expect(DeepAnalyze.sceneCaptionText("  A desk. ") == "A desk.")
    }

    @Test("scene token cap defaults to 128 and accepts a bounded override")
    func sceneCap() {
        #expect(DeepAnalyze.sceneMaxTokens(environment: [:]) == 128)
        #expect(DeepAnalyze.sceneMaxTokens(environment: ["FILEID_VLM_SCENE_MAX_TOKENS": "200"]) == 200)
        #expect(DeepAnalyze.sceneMaxTokens(environment: ["FILEID_VLM_SCENE_MAX_TOKENS": "5"]) == 128)
    }

    @Test("loop guard defaults on and switches off with 0")
    func switches() {
        #expect(VLMGreedyDecoder.loopGuardEnabled(environment: [:]))
        #expect(!VLMGreedyDecoder.loopGuardEnabled(environment: ["FILEID_VLM_LOOP_GUARD": "0"]))
    }
}

@Suite("Photo caption reply format")
struct VLMReplyFormatTests {
    @Test("Qwen3-VL 2B defaults to the one-line reply, other models to sections, and the switch overrides both")
    func defaultFormat() {
        #expect(DeepAnalyze.replyFormat(model: .qwen3VL2B, environment: [:]) == .compact)
        #expect(DeepAnalyze.replyFormat(model: .qwen3VL4B, environment: [:]) == .sections)
        #expect(DeepAnalyze.replyFormat(model: .qwen3VL8B, environment: [:]) == .sections)
        #expect(DeepAnalyze.replyFormat(model: nil, environment: [:]) == .sections)
        #expect(DeepAnalyze.replyFormat(model: .qwen3VL2B, environment: ["FILEID_VLM_FORMAT": "sections"]) == .sections)
        #expect(DeepAnalyze.replyFormat(model: .qwen3VL4B, environment: ["FILEID_VLM_FORMAT": "compact"]) == .compact)
        #expect(DeepAnalyze.replyFormat(model: .qwen3VL2B, environment: ["FILEID_VLM_FORMAT": "bogus"]) == .compact)
        #expect(DeepAnalyze.replyFormat(model: .qwen3VL4B, environment: ["FILEID_VLM_FORMAT": "bogus"]) == .sections)
    }

    @Test("captions use the library stream except for the compact-line model, and the switch overrides both")
    func captionDecoder() {
        #expect(DeepAnalyze.captionUsesGreedy(model: .qwen3VL2B, environment: [:]))
        #expect(!DeepAnalyze.captionUsesGreedy(model: .qwen3VL4B, environment: [:]))
        #expect(!DeepAnalyze.captionUsesGreedy(model: nil, environment: [:]))
        #expect(DeepAnalyze.captionUsesGreedy(model: .qwen3VL4B, environment: ["FILEID_VLM_DECODER": "greedy"]))
        #expect(!DeepAnalyze.captionUsesGreedy(model: .qwen3VL2B, environment: ["FILEID_VLM_DECODER": "library"]))
    }

    @Test("the compact prompt asks for one line of three fields")
    func compactPrompt() {
        let prompt = DeepAnalyze.analysisSystemPrompt(
            mediaKind: .image, fileExtension: "jpg", hasRaster: true, faceNames: ["Kyle"],
            includeTags: true, format: .compact
        )
        #expect(prompt.contains("description | filename | tags"))
        #expect(prompt.contains(DeepAnalyze.descriptionRules))
        #expect(prompt.contains("Known people in this photo: Kyle."))
        #expect(!prompt.contains("DESCRIPTION:"))
        let untagged = DeepAnalyze.analysisSystemPrompt(
            mediaKind: .image, fileExtension: "jpg", hasRaster: true, faceNames: [],
            includeTags: false, format: .compact
        )
        #expect(untagged.contains("EXACTLY two sections"))
    }

    @Test("a compact reply parses into description, filename and tags")
    func parsesCompact() {
        let parsed = DeepAnalyze.parse(
            rawOutput: "A boy in a blue \"Athletics\" uniform holds a red bat on a grassy field. | boy-holding-red-bat | baseball bat, youth baseball\n",
            format: .compact
        )
        #expect(parsed.description == "A boy in a blue \"Athletics\" uniform holds a red bat on a grassy field.")
        #expect(parsed.proposedName == "boy-holding-red-bat")
        #expect(parsed.tags == ["baseball bat", "youth baseball"])
        #expect(parsed.hasTagSection)
    }

    @Test("labelled or partial compact replies still parse, and a sections reply falls back")
    func compactFallbacks() {
        let labelled = DeepAnalyze.parse(rawOutput: "Description: A sundial in a rose garden. | Filename: sundial-rose-garden | tags: sundial", format: .compact)
        #expect(labelled.description == "A sundial in a rose garden.")
        #expect(labelled.proposedName == "sundial-rose-garden")
        #expect(labelled.tags == ["sundial"])
        let noTags = DeepAnalyze.parse(rawOutput: "A gazebo in a garden. | garden-gazebo", format: .compact)
        #expect(noTags.proposedName == "garden-gazebo")
        #expect(!noTags.hasTagSection)
        let sections = DeepAnalyze.parse(rawOutput: "DESCRIPTION: A lake.\nFILENAME: mountain-lake\nTAGS: lake", format: .compact)
        #expect(sections.description == "A lake.")
        #expect(sections.proposedName == "mountain-lake")
        #expect(sections.tags == ["lake"])
    }

    @Test("a compact reply is complete once its line holds every field and ends")
    func compactCompletion() {
        #expect(!DeepAnalyze.isCompleteReply("A dog runs. | dog-running | dog", format: .compact, includeTags: true))
        #expect(!DeepAnalyze.isCompleteReply("A dog runs. | dog-running\n", format: .compact, includeTags: true))
        #expect(DeepAnalyze.isCompleteReply("A dog runs. | dog-running | dog\n", format: .compact, includeTags: true))
        #expect(DeepAnalyze.isCompleteReply("A dog runs. | dog-running\n", format: .compact, includeTags: false))
    }
}
