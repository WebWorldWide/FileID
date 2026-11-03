import Testing
import Foundation
@testable import FileIDEngine

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
