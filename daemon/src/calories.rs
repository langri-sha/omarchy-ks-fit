//! Energy expenditure estimates for treadmill walking.

/// Treadmill walking at 0% grade from the 2024 Adult Compendium, as
/// `(lowest km/h, MET)`, with the gaps between its mph bins closed.
const METS: [(f64, f64); 10] = [
    (0.0, 2.1),
    (1.6, 2.3),
    (1.9, 2.8),
    (3.2, 3.0),
    (4.0, 3.5),
    (4.8, 3.8),
    (5.6, 4.8),
    (6.4, 5.8),
    (7.2, 6.8),
    (8.0, 8.3),
];

/// The metabolic equivalent of walking at `kmh`.
pub fn met(kmh: f64) -> f64 {
    METS.iter()
        .rev()
        .find(|(from, _)| kmh >= *from)
        .map_or(METS[0].1, |(_, met)| *met)
}

/// Gross energy in kcal for walking `seconds` at `kmh` with a body mass of
/// `weight_kg`.
pub fn kcal(kmh: f64, weight_kg: f64, seconds: u32) -> f64 {
    met(kmh) * 3.5 * weight_kg / 200.0 * f64::from(seconds) / 60.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_bin_starts_at_its_lower_edge() {
        let edges = [
            (1.6, 2.1, 2.3),
            (1.9, 2.3, 2.8),
            (3.2, 2.8, 3.0),
            (4.0, 3.0, 3.5),
            (4.8, 3.5, 3.8),
            (5.6, 3.8, 4.8),
            (6.4, 4.8, 5.8),
            (7.2, 5.8, 6.8),
            (8.0, 6.8, 8.3),
        ];
        for (edge, below, at) in edges {
            assert_eq!(met(edge - 0.1), below, "below {edge}");
            assert_eq!(met(edge), at, "at {edge}");
        }
    }

    #[test]
    fn edges_hold_for_speeds_in_tenths_and_averaged_from_distance() {
        assert_eq!(met(f64::from(16_u8) / 10.0), 2.3);
        assert_eq!(met(f64::from(32_u8) / 10.0), 3.0);
        assert_eq!(met(f64::from(4_u32) * 36.0 / 90.0), 2.3);
    }

    #[test]
    fn standing_still_and_nonsense_speeds_use_the_slowest_bin() {
        assert_eq!(met(0.0), 2.1);
        assert_eq!(met(-1.0), 2.1);
        assert_eq!(met(f64::NAN), 2.1);
        assert_eq!(met(25.0), 8.3);
    }

    #[test]
    fn a_minute_at_four_kmh_for_eighty_kg_burns_four_point_nine_kcal() {
        assert!((kcal(4.0, 80.0, 60) - 4.9).abs() < 1e-12);
        assert!((kcal(6.0, 80.0, 30) - 3.36).abs() < 1e-12);
    }

    #[test]
    fn no_time_burns_nothing() {
        assert_eq!(kcal(4.0, 80.0, 0), 0.0);
    }
}
