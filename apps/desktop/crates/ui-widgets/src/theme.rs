use gpui_kit_material::color::{Color, ColorRole as C, PaletteId, Scheme};

/// One color contract for the shell, independent pages and Material controls.
pub fn workspace_scheme(dark: bool) -> Scheme {
    let (background, surface, inset, border, text, muted, accent, selected, on_accent) = if dark {
        (
            0x101113, 0x17191c, 0x22252a, 0x30343a, 0xedf0f3, 0xa5adb9, 0x9bbcff, 0x23314a,
            0x142442,
        )
    } else {
        (
            0xffffff, 0xf7f8fa, 0xeff2f6, 0xe0e5ed, 0x20242b, 0x626d7d, 0x245bd7, 0xeaf0ff,
            0xffffff,
        )
    };
    let mut scheme = Scheme::from_palette(if dark {
        PaletteId::Ink
    } else {
        PaletteId::Paper
    });
    for (role, color) in [
        (C::SURFACE, background),
        (C::SURFACE_DIM, background),
        (C::SURFACE_BRIGHT, surface),
        (C::SURFACE_CONTAINER_LOWEST, background),
        (C::SURFACE_CONTAINER_LOW, surface),
        (C::SURFACE_CONTAINER, surface),
        (C::SURFACE_CONTAINER_HIGH, inset),
        (C::SURFACE_CONTAINER_HIGHEST, inset),
        (C::SURFACE_VARIANT, inset),
        (C::INSET, inset),
        (C::OUTLINE_VARIANT, border),
        (C::CARD_LINE, border),
        (C::OUTLINE, muted),
        (C::ON_SURFACE, text),
        (C::ON_SURFACE_VARIANT, muted),
        (C::FAINT, muted),
        (C::PRIMARY, accent),
        (C::ON_PRIMARY, on_accent),
        (C::PRIMARY_CONTAINER, selected),
        (C::ON_PRIMARY_CONTAINER, accent),
        // Material tonal toggles use SECONDARY for their selected state.
        (C::SECONDARY, selected),
        (C::ON_SECONDARY, accent),
        (C::SECONDARY_CONTAINER, selected),
        (C::ON_SECONDARY_CONTAINER, accent),
        (C::SELECTION, selected),
        (C::READING_SELECTION, selected),
    ] {
        scheme = scheme.with_role(role, Color::hex(color));
    }
    scheme
}
