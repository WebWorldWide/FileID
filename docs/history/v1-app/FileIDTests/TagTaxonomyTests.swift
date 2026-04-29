import XCTest
@testable import FileID

final class TagTaxonomyTests: XCTestCase {

    func testKnownLabelsRewritten() {
        // The user-visible win of TagTaxonomy: "Optical Equipment" reads as
        // "Glasses" on the file card. These mappings are part of the contract;
        // any future regression here is a UX regression.
        XCTAssertEqual(TagTaxonomy.humanize("optical_equipment"),  "Glasses")
        XCTAssertEqual(TagTaxonomy.humanize("Optical_Equipment"),  "Glasses")
        XCTAssertEqual(TagTaxonomy.humanize("optical equipment"),  "Glasses")
        XCTAssertEqual(TagTaxonomy.humanize("domesticated_animal"),"Pet")
        XCTAssertEqual(TagTaxonomy.humanize("bottled_and_jarred_packaged_foods"),
                       "Packaged Food")
        XCTAssertEqual(TagTaxonomy.humanize("motor_vehicle"), "Vehicle")
    }

    func testUnknownLabelsPassThroughUnchanged() {
        // Internal tag contracts (Tax_Document, Invoice, Screenshot, year tags
        // like 2024_12, PDF, Large_Document, CLIP-derived labels) MUST pass
        // through unchanged so downstream category routing still works.
        XCTAssertEqual(TagTaxonomy.humanize("Tax_Document"),    "Tax_Document")
        XCTAssertEqual(TagTaxonomy.humanize("Invoice"),         "Invoice")
        XCTAssertEqual(TagTaxonomy.humanize("Screenshot"),      "Screenshot")
        XCTAssertEqual(TagTaxonomy.humanize("2024_12"),         "2024_12")
        XCTAssertEqual(TagTaxonomy.humanize("PDF"),             "PDF")
        XCTAssertEqual(TagTaxonomy.humanize("Large_Document"),  "Large_Document")
        XCTAssertEqual(TagTaxonomy.humanize("Sunset"),          "Sunset")
    }

    func testHumanizeArrayDedupesPreservingOrder() {
        let input  = ["optical_equipment", "Glasses", "domesticated_animal", "Pet", "Sunset"]
        let output = TagTaxonomy.humanize(input)
        // "optical_equipment" -> "Glasses" (first)
        // "Glasses"           -> "Glasses" (dup, dropped)
        // "domesticated_animal" -> "Pet" (first)
        // "Pet"               -> "Pet" (dup, dropped)
        // "Sunset"            -> "Sunset" (first)
        XCTAssertEqual(output, ["Glasses", "Pet", "Sunset"])
    }

    func testEmptyInputReturnsEmpty() {
        XCTAssertEqual(TagTaxonomy.humanize([]), [])
        XCTAssertEqual(TagTaxonomy.humanize([""]), [])
    }

    func testCollapsesMultipleUnderscores() {
        // Upstream Vision concatenations occasionally produce double
        // underscores; the key normalizer should still match the dictionary.
        XCTAssertEqual(TagTaxonomy.humanize("optical__equipment"), "Glasses")
    }
}
