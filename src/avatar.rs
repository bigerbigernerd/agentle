//! Sticker-style avatars, generated from a seed. Same seed, same face.

use sha2::{Digest, Sha256};

const BGS: [&str; 7] = ["#B7F27A", "#FFE14D", "#8FB0FF", "#FF8FB4", "#C9B6FF", "#FFB067", "#6FE0C6"];
const FACES: [&str; 6] = ["#FFE14D", "#FFFFFF", "#FFC2D9", "#B7F27A", "#BFD9FF", "#FFD9AE"];
const ACCENTS: [&str; 4] = ["#FF3D7F", "#2D5BFF", "#FF9F45", "#7BC74D"];
const INK: &str = "#111111";

pub fn svg(seed: &str) -> String {
    let h = Sha256::digest(seed.as_bytes());
    let b = |i: usize| h[i] as usize;
    let bg = BGS[b(0) % BGS.len()];
    let mut face = FACES[b(1) % FACES.len()];
    if face == bg {
        face = FACES[(b(1) + 1) % FACES.len()];
    }
    let accent = ACCENTS[b(7) % ACCENTS.len()];
    let shape = b(2) % 4;
    let eyes = b(3) % 6;
    let mouth = b(4) % 5;
    let cheeks = b(5) % 2 == 0;
    let acc = if shape == 2 { 0 } else { b(6) % 5 };

    let stroke = format!(r#"stroke="{INK}" stroke-width="4" stroke-linejoin="round" stroke-linecap="round""#);
    let mut s = String::with_capacity(2048);
    s.push_str(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 120" width="120" height="120">"#);
    s.push_str(&format!(r#"<rect width="120" height="120" fill="{bg}"/>"#));

    // things that sit behind the head
    match acc {
        1 => s.push_str(&format!(
            r#"<path d="M60 32 L60 15" fill="none" {stroke}/><circle cx="60" cy="12" r="6" fill="{accent}" {stroke}/>"#
        )),
        3 => s.push_str(&format!(
            r##"<path d="M60 32 Q60 24 60 18" fill="none" {stroke}/><path d="M60 19 Q48 7 41 16 Q50 25 60 19 Z" fill="#7BC74D" {stroke}/><path d="M60 19 Q72 7 79 16 Q70 25 60 19 Z" fill="#7BC74D" {stroke}/>"##
        )),
        4 => s.push_str(&format!(
            r#"<ellipse cx="60" cy="17" rx="22" ry="6" fill="none" stroke="{INK}" stroke-width="9"/><ellipse cx="60" cy="17" rx="22" ry="6" fill="none" stroke="{accent}" stroke-width="4"/>"#
        )),
        _ => {}
    }

    match shape {
        0 => s.push_str(&format!(r#"<circle cx="60" cy="67" r="36" fill="{face}" {stroke}/>"#)),
        1 => s.push_str(&format!(r#"<rect x="24" y="31" width="72" height="72" rx="20" fill="{face}" {stroke}/>"#)),
        2 => s.push_str(&format!(
            r#"<path d="M46 37 L36 18 M74 37 L84 18" fill="none" {stroke}/><circle cx="36" cy="16" r="5" fill="{accent}" {stroke}/><circle cx="84" cy="16" r="5" fill="{accent}" {stroke}/><rect x="18" y="37" width="84" height="64" rx="15" fill="{face}" {stroke}/>"#
        )),
        _ => s.push_str(&format!(
            r#"<path d="M60 30 C84 30 98 46 96 68 C94 92 80 103 60 103 C38 103 24 92 24 68 C24 44 38 30 60 30 Z" fill="{face}" {stroke}/>"#
        )),
    }

    let ey = 63;
    match eyes {
        0 => s.push_str(&format!(
            r#"<ellipse cx="46" cy="{ey}" rx="5" ry="8" fill="{INK}"/><ellipse cx="74" cy="{ey}" rx="5" ry="8" fill="{INK}"/>"#
        )),
        1 => s.push_str(&format!(
            r##"<circle cx="46" cy="{ey}" r="6" fill="{INK}"/><circle cx="74" cy="{ey}" r="6" fill="{INK}"/><circle cx="48" cy="61" r="2" fill="#FFFFFF"/><circle cx="76" cy="61" r="2" fill="#FFFFFF"/>"##
        )),
        2 => s.push_str(&format!(r#"<path d="M39 66 Q46 55 53 66 M67 66 Q74 55 81 66" fill="none" {stroke}/>"#)),
        3 => s.push_str(&format!(
            r#"<ellipse cx="46" cy="{ey}" rx="5" ry="8" fill="{INK}"/><path d="M67 64 Q74 57 81 64" fill="none" {stroke}/>"#
        )),
        4 => s.push_str(&format!(
            r##"<rect x="33" y="54" width="54" height="17" rx="8.5" fill="{INK}"/><rect x="41" y="59" width="9" height="4" rx="2" fill="#FFFFFF"/><rect x="70" y="59" width="9" height="4" rx="2" fill="#FFFFFF"/>"##
        )),
        _ => s.push_str(&format!(r#"<path d="M39 64 L53 64 M67 64 L81 64" fill="none" {stroke}/>"#)),
    }

    if cheeks {
        s.push_str(r##"<ellipse cx="36" cy="78" rx="6" ry="4" fill="#FF6F9C" opacity="0.75"/><ellipse cx="84" cy="78" rx="6" ry="4" fill="#FF6F9C" opacity="0.75"/>"##);
    }

    match mouth {
        1 => s.push_str(&format!(r#"<path d="M50 81 Q60 91 70 81" fill="none" {stroke}/>"#)),
        2 => s.push_str(&format!(r#"<ellipse cx="60" cy="85" rx="4.5" ry="5.5" fill="{INK}"/>"#)),
        3 => s.push_str(&format!(r#"<path d="M50 81 Q55 87 60 81 Q65 87 70 81" fill="none" {stroke}/>"#)),
        4 => s.push_str(&format!(r#"<path d="M48 80 Q60 97 72 80 Z" fill="{INK}" {stroke}/>"#)),
        _ => {}
    }

    if acc == 2 {
        s.push_str(&format!(
            r#"<path d="M80 33 L67 24 L67 42 Z M80 33 L93 24 L93 42 Z" fill="{accent}" {stroke}/><circle cx="80" cy="33" r="4" fill="{accent}" {stroke}/>"#
        ));
    }
    s.push_str("</svg>");
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn deterministic_and_wellformed() {
        let a = super::svg("night-owl");
        assert_eq!(a, super::svg("night-owl"));
        assert!(a.starts_with("<svg") && a.ends_with("</svg>"));
        assert_ne!(a, super::svg("lime"));
    }
}
