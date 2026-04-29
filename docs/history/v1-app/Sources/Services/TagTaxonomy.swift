import Foundation

// Apple Vision's VNClassifyImageRequest emits taxonomy labels that read like
// database keys to a human ("optical_equipment", "bottled_and_jarred_packaged_foods").
// This layer rewrites the ones users actually see in the Library into everyday
// words. Unknown labels pass through unchanged so internal tag contracts
// ("Tax_Document", "Invoice", "Screenshot", date tags like "2024_12") keep
// working in downstream category routing.
enum TagTaxonomy {

    // Lookup key is the Vision label normalized via `key(for:)`:
    // lowercased, underscores collapsed to single, spaces → underscores.
    // That lets us accept whatever casing upstream hands us — "Optical_Equipment",
    // "optical equipment", or "optical_equipment" all hit the same entry.
    private static let humanReplacements: [String: String] = [
        // Eyewear / accessories
        "optical_equipment":                "Glasses",
        "optical_instrument":               "Glasses",
        "eyewear":                          "Glasses",
        "personal_accessory":               "Accessory",
        "fashion_accessory":                "Accessory",
        "jewelry":                          "Jewelry",
        "headgear":                         "Hat",
        "footwear":                         "Shoes",

        // Apparel
        "outerwear":                        "Clothing",
        "garment":                          "Clothing",
        "undergarment":                     "Clothing",
        "sportswear":                       "Activewear",

        // Food & drink
        "bottled_and_jarred_packaged_foods":"Packaged Food",
        "packaged_foods":                   "Packaged Food",
        "bakery_product":                   "Baked Goods",
        "confectionery":                    "Sweets",
        "dairy_product":                    "Dairy",
        "alcoholic_beverage":               "Drink",
        "non_alcoholic_beverage":           "Drink",
        "fruit":                            "Fruit",
        "vegetable":                        "Vegetable",

        // Home & everyday objects
        "miscellaneous_daily_necessities":  "Household Item",
        "household_cleaning_supply":        "Cleaning Supply",
        "kitchen_utensil":                  "Kitchenware",
        "tableware":                        "Dishware",
        "furniture":                        "Furniture",
        "home_appliance":                   "Appliance",
        "major_appliance":                  "Appliance",
        "small_appliance":                  "Appliance",
        "office_supply":                    "Office Supply",
        "writing_instrument":               "Pen",

        // Electronics
        "electronic_device":                "Electronics",
        "consumer_electronics":             "Electronics",
        "computer_peripheral":              "Computer Accessory",
        "audio_equipment":                  "Audio Gear",

        // Vehicles
        "motor_vehicle":                    "Vehicle",
        "land_vehicle":                     "Vehicle",
        "watercraft":                       "Boat",
        "aircraft":                         "Aircraft",

        // Nature
        "natural_phenomenon":               "Nature",
        "terrestrial_plant":                "Plant",
        "aquatic_plant":                    "Plant",
        "geological_formation":             "Landscape",
        "body_of_water":                    "Water",

        // Animals
        "domesticated_animal":              "Pet",
        "aquatic_animal":                   "Aquatic Animal",
        "wild_animal":                      "Wildlife",

        // Media / docs
        "publication":                      "Book",
        "printed_material":                 "Document",

        // People
        "human_body_part":                  "Body",
        "facial_feature":                   "Face",
    ]

    private static func key(for label: String) -> String {
        let collapsed = label
            .lowercased()
            .replacingOccurrences(of: " ", with: "_")
        // Collapse multiple underscores in case upstream concatenation produces "a__b"
        var out = ""
        var lastWasUnderscore = false
        for ch in collapsed {
            if ch == "_" {
                if !lastWasUnderscore { out.append(ch) }
                lastWasUnderscore = true
            } else {
                out.append(ch)
                lastWasUnderscore = false
            }
        }
        return out
    }

    static func humanize(_ label: String) -> String {
        humanReplacements[key(for: label)] ?? label
    }

    // Dedups + drops empty. Preserves first-occurrence order so the most
    // confident Vision hit stays at the front of aiTags.
    static func humanize(_ labels: [String]) -> [String] {
        var seen = Set<String>()
        var out: [String] = []
        out.reserveCapacity(labels.count)
        for raw in labels {
            let mapped = humanize(raw)
            guard !mapped.isEmpty, seen.insert(mapped).inserted else { continue }
            out.append(mapped)
        }
        return out
    }
}
