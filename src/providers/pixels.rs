//! Pixel-format conversions shared by decoders that don't natively emit RGBA8.

/// Widens tightly-packed RGB triples to RGBA8, filling alpha as opaque.
pub fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for chunk in rgb.chunks_exact(3) {
        out.extend_from_slice(chunk);
        out.push(255);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_opaque_alpha_to_each_triple() {
        let rgb: [u8; 6] = [255, 0, 0, 0, 255, 0];
        assert_eq!(rgb_to_rgba(&rgb), vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }
}
