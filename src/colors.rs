//! Colour values written in code (`#1e90ff`, `rgb(0 0 0 / 50%)`, `hsl(210, 50%, 40%)`), found
//! line by line for the swatches drawn next to them. Pure string scanning, nothing cached.

use std::ops::Range;

/// A colour in a line: its char columns and its value (r, g, b, a in 0…1).
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub cols: Range<usize>,
    pub rgba: [f32; 4],
}

/// Colours in `line`. `in_string(col)` says whether a column is inside a string literal; in
/// stylesheets (`css`) colours count anywhere after a `property:`.
pub fn find(line: &str, css: bool, in_string: impl Fn(usize) -> bool) -> Vec<Found> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let found = if c == '#' {
            hex_at(&chars, i)
        } else if c.is_ascii_alphabetic() && (i == 0 || !is_word(chars[i - 1])) {
            func_at(&chars, i)
        } else {
            None
        };
        if let Some(f) = found {
            let ok = if css { chars[..i].contains(&':') && next_non_space(&chars, f.cols.end) != Some('{') } else { in_string(i) };
            if ok {
                i = f.cols.end;
                out.push(f);
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '$'
}

fn next_non_space(chars: &[char], from: usize) -> Option<char> {
    chars[from.min(chars.len())..].iter().copied().find(|c| !c.is_whitespace())
}

/// `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`, not part of a longer word (`a#b`, `&#123;`).
fn hex_at(chars: &[char], i: usize) -> Option<Found> {
    if i > 0 && (is_word(chars[i - 1]) || chars[i - 1] == '&') {
        return None;
    }
    let digits: String = chars[i + 1..].iter().take_while(|c| c.is_ascii_hexdigit()).collect();
    let n = digits.len();
    if !matches!(n, 3 | 4 | 6 | 8) || chars.get(i + 1 + n).is_some_and(|c| is_word(*c)) {
        return None;
    }
    let v: Vec<u8> = digits.bytes().map(|b| (b as char).to_digit(16).unwrap_or(0) as u8).collect();
    let pair = |k: usize| (v[k] * 16 + v[k + 1]) as f32 / 255.;
    let single = |k: usize| (v[k] * 17) as f32 / 255.;
    let rgba = match n {
        3 => [single(0), single(1), single(2), 1.],
        4 => [single(0), single(1), single(2), single(3)],
        6 => [pair(0), pair(2), pair(4), 1.],
        _ => [pair(0), pair(2), pair(4), pair(6)],
    };
    Some(Found { cols: i..i + 1 + n, rgba })
}

/// `rgb()`, `rgba()`, `hsl()` or `hsla()` with plain numbers (commas or spaces, `/ alpha`).
fn func_at(chars: &[char], i: usize) -> Option<Found> {
    let name: String = chars[i..].iter().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_lowercase();
    if !matches!(name.as_str(), "rgb" | "rgba" | "hsl" | "hsla") || chars.get(i + name.len()) != Some(&'(') {
        return None;
    }
    let open = i + name.len();
    let close = open + chars[open..].iter().position(|c| *c == ')')?;
    let inner: String = chars[open + 1..close].iter().collect();
    let parts: Vec<&str> = inner.split(|c: char| c == ',' || c == '/' || c.is_whitespace()).filter(|p| !p.is_empty()).collect();
    if !(3..=4).contains(&parts.len()) {
        return None;
    }
    // A number, or a percentage of `full`.
    let num = |s: &str, full: f32| -> Option<f32> {
        match s.strip_suffix('%') {
            Some(p) => p.parse::<f32>().ok().map(|v| v / 100. * full),
            None => s.trim_end_matches("deg").parse::<f32>().ok(),
        }
    };
    let alpha = match parts.get(3) {
        Some(a) => num(a, 1.)?.clamp(0., 1.),
        None => 1.,
    };
    let rgba = if name.starts_with("rgb") {
        let ch = |k: usize| num(parts[k], 255.).map(|v| (v / 255.).clamp(0., 1.));
        [ch(0)?, ch(1)?, ch(2)?, alpha]
    } else {
        let h = num(parts[0], 360.)?.rem_euclid(360.) / 360.;
        // Saturation and lightness: `50%`, or a bare `50` (CSS Color 4).
        let s = num(parts[1].trim_end_matches('%'), 1.)? / 100.;
        let l = num(parts[2].trim_end_matches('%'), 1.)? / 100.;
        let (r, g, b) = hsl_to_rgb(h, s.clamp(0., 1.), l.clamp(0., 1.));
        [r, g, b, alpha]
    };
    Some(Found { cols: i..close + 1, rgba })
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s == 0. {
        return (l, l, l);
    }
    let q = if l < 0.5 { l * (1. + s) } else { l + s - l * s };
    let p = 2. * l - q;
    let t = |mut t: f32| {
        if t < 0. {
            t += 1.;
        }
        if t > 1. {
            t -= 1.;
        }
        if t < 1. / 6. {
            p + (q - p) * 6. * t
        } else if t < 0.5 {
            q
        } else if t < 2. / 3. {
            p + (q - p) * (2. / 3. - t) * 6.
        } else {
            p
        }
    };
    (t(h + 1. / 3.), t(h), t(h - 1. / 3.))
}

#[cfg(test)]
mod tests {
    use super::find;

    fn all(line: &str) -> Vec<(String, [u8; 4])> {
        find(line, false, |_| true)
            .into_iter()
            .map(|f| {
                let text: String = line.chars().skip(f.cols.start).take(f.cols.len()).collect();
                (text, f.rgba.map(|v| (v * 255.).round() as u8))
            })
            .collect()
    }

    #[test]
    fn finds_hex_and_functions() {
        assert_eq!(all("backgroundColor: '#1e90ff', color: \"#fff\""), vec![("#1e90ff".into(), [30, 144, 255, 255]), ("#fff".into(), [255, 255, 255, 255])]);
        assert_eq!(all("shadowColor: 'rgba(0, 0, 0, 0.5)'"), vec![("rgba(0, 0, 0, 0.5)".into(), [0, 0, 0, 128])]);
        assert_eq!(all("rgb(255 0 0 / 50%)")[0].1, [255, 0, 0, 128]);
        assert_eq!(all("hsl(120, 100%, 50%)")[0].1, [0, 255, 0, 255]);
        assert_eq!(all("#ff000080")[0].1, [255, 0, 0, 128]);
    }

    #[test]
    fn skips_things_that_only_look_like_colours() {
        assert!(all("issue#123 a#bc &#123; #12345 #abcdefg").is_empty());
        assert!(all("rgb(1, 2) myrgb(1,2,3)").is_empty());
        // Outside strings in scripts: a private field, not a colour.
        assert!(find("this.#abc = 1", false, |_| false).is_empty());
        // Stylesheets: values after a property, not id selectors.
        assert_eq!(find("  color: #abc;", true, |_| false).len(), 1);
        assert!(find("#add { color: red }", true, |_| false).is_empty());
    }
}
