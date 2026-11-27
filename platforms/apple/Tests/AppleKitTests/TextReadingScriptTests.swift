import Testing
@testable import FileIDImaging

@Suite("Text reading across scripts")
struct TextReadingScriptTests {
    @Test func printedNonLatinWordsAreNotGarbledForHavingNoVowels() {
        for word in ["Привет", "Здравствуйте", "Καλημέρα", "東京都渋谷区", "한국어문장"] {
            #expect(!TextReading.isGarbledWord(Substring(word)), "\(word)")
        }
        #expect(TextReading.garbled("Квартальный отчёт по продажам Итого к оплате").count == 0)
        #expect(TextReading.isGarbledWord("Swvbda"))
    }

    @Test func aPrintedRussianPageTheEnglishFastReadMissedIsNotHandwriting() {
        let careful = "Квартальный отчёт по продажам итого к оплате"
        #expect(!TextReading.isHandwritten(fastText: "", carefulText: careful, tags: [], paperLike: true))
        #expect(TextReading.isHandwritten(fastText: "", carefulText: careful, tags: ["handwriting"], paperLike: true))
        let latin = "Buy milk and eggs from the corner shop today"
        #expect(TextReading.isHandwritten(fastText: "", carefulText: latin, tags: [], paperLike: true))
    }
}
