//! COCO class index → GIAP event label mapping.
//!
//! The recommended models (YOLOX-Nano, NanoDet-Plus) are trained on COCO's
//! 80 classes. GIAP only cares about a handful of home-relevant labels, so
//! everything else maps to `None` and the event stays plain `"motion"`.
//!
//! "package" is NOT a COCO class — until a fine-tuned model ships, luggage
//! classes (backpack / handbag / suitcase) act as the proxy, which covers
//! the common porch-delivery silhouettes.

/// GIAP label for a COCO class index, or `None` when the class is not
/// home-relevant (event falls back to `"motion"`).
pub fn coco_to_giap_label(class_idx: usize) -> Option<&'static str> {
    match class_idx {
        0 => Some("person"),
        // bird, cat, dog
        14..=16 => Some("pet"),
        // backpack, handbag, suitcase — the "package" proxy classes
        24 | 26 | 28 => Some("package"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_home_relevant_classes_and_ignores_the_rest() {
        assert_eq!(coco_to_giap_label(0), Some("person"));
        assert_eq!(coco_to_giap_label(15), Some("pet")); // cat
        assert_eq!(coco_to_giap_label(16), Some("pet")); // dog
        assert_eq!(coco_to_giap_label(28), Some("package")); // suitcase
        assert_eq!(coco_to_giap_label(2), None); // car — not home-relevant
        assert_eq!(coco_to_giap_label(79), None); // toothbrush
    }
}
