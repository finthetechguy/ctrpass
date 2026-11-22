//! Names of games known to use StreetPass, for titles a store holds no name for.

const NAMES: &[(u32, &str)] = &[
    (0x0002_0500, "Nintendo 3DS Sound"),
    (0x0002_0800, "Mii Plaza"),
    (0x0003_0600, "Mario Kart 7"),
    (0x0003_0B00, "nintendogs + cats"),
    (0x0005_1600, "Nintendo Letter Box"),
    (0x0005_3F00, "Super Mario 3D Land"),
    (0x0007_AD00, "New Super Mario Bros. 2"),
    (0x0007_C800, "Mario Tennis Open"),
    (0x0008_C500, "Tomodachi Life"),
    (0x0012_EC00, "Tetris Ultimate"),
    (0x0014_1000, "Pokémon Shuffle"),
    (0x001A_0400, "Super Mario Maker"),
];

pub fn name(title: u32) -> Option<&'static str> {
    NAMES.iter().find(|(t, _)| *t == title).map(|(_, n)| *n)
}

pub fn all() -> &'static [(u32, &'static str)] {
    NAMES
}

#[cfg(test)]
mod tests {
    #[test]
    fn looks_up() {
        assert_eq!(super::name(0x20800), Some("Mii Plaza"));
        assert_eq!(super::name(1), None);
    }
}
