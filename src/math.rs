//! A small LaTeX → Unicode "prettifier" for inline and display math.
//!
//! This is *not* a full typesetting engine: it maps common LaTeX control
//! sequences to their Unicode equivalents and converts simple `^`/`_` groups to
//! Unicode super/subscripts, which covers the overwhelming majority of everyday
//! note math. Anything it doesn't understand is left as-is.

/// Convert a LaTeX math snippet to a Unicode string.
pub fn latex_to_unicode(tex: &str) -> String {
    let mut out = String::with_capacity(tex.len());
    let chars: Vec<char> = tex.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            // Control word or symbol.
            let mut j = i + 1;
            let mut name = String::new();
            while j < chars.len() && chars[j].is_ascii_alphabetic() {
                name.push(chars[j]);
                j += 1;
            }
            if name.is_empty() {
                // A single escaped char (\%, \{, ...).
                if j < chars.len() {
                    out.push(chars[j]);
                    i = j + 1;
                } else {
                    i = j;
                }
                continue;
            }
            match symbol(&name) {
                Some(sym) => out.push_str(sym),
                None => {
                    // Unknown control word: keep the name (drop the backslash).
                    out.push_str(&name);
                }
            }
            i = j;
            continue;
        }
        if (c == '^' || c == '_') && i + 1 < chars.len() {
            let (text, next) = read_group(&chars, i + 1);
            let mapped = map_scripts(&text, c == '^');
            out.push_str(&mapped);
            i = next;
            continue;
        }
        if c == '{' || c == '}' {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Read a `{...}` group (or a single char) starting at `start`.
fn read_group(chars: &[char], start: usize) -> (String, usize) {
    if start < chars.len() && chars[start] == '{' {
        let mut depth = 0usize;
        let mut i = start;
        let mut s = String::new();
        while i < chars.len() {
            match chars[i] {
                '{' => {
                    depth += 1;
                    if depth > 1 {
                        s.push('{');
                    }
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return (s, i + 1);
                    }
                    s.push('}');
                }
                ch => s.push(ch),
            }
            i += 1;
        }
        (s, i)
    } else if start < chars.len() {
        (chars[start].to_string(), start + 1)
    } else {
        (String::new(), start)
    }
}

fn map_scripts(text: &str, sup: bool) -> String {
    let table: &[(char, char)] = if sup {
        &[
            ('0', '⁰'),
            ('1', '¹'),
            ('2', '²'),
            ('3', '³'),
            ('4', '⁴'),
            ('5', '⁵'),
            ('6', '⁶'),
            ('7', '⁷'),
            ('8', '⁸'),
            ('9', '⁹'),
            ('+', '⁺'),
            ('-', '⁻'),
            ('=', '⁼'),
            ('(', '⁽'),
            (')', '⁾'),
            ('n', 'ⁿ'),
            ('i', 'ⁱ'),
            ('x', 'ˣ'),
        ]
    } else {
        &[
            ('0', '₀'),
            ('1', '₁'),
            ('2', '₂'),
            ('3', '₃'),
            ('4', '₄'),
            ('5', '₅'),
            ('6', '₆'),
            ('7', '₇'),
            ('8', '₈'),
            ('9', '₉'),
            ('+', '₊'),
            ('-', '₋'),
            ('=', '₌'),
            ('(', '₍'),
            (')', '₎'),
            ('a', 'ₐ'),
            ('e', 'ₑ'),
            ('i', 'ᵢ'),
            ('n', 'ₙ'),
            ('x', 'ₓ'),
        ]
    };
    let converted: Option<Vec<char>> = text
        .chars()
        .map(|c| table.iter().find(|(k, _)| *k == c).map(|(_, v)| *v))
        .collect();
    match converted {
        Some(v) => v.into_iter().collect(),
        None => {
            // Not fully mappable: fall back to `<base>^text` / `<base>_text`.
            let marker = if sup { '^' } else { '_' };
            if text.chars().count() == 1 {
                format!("{marker}{text}")
            } else {
                format!("{marker}({text})")
            }
        }
    }
}

fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        // Greek (lower)
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "rho" => "ρ",
        "sigma" => "σ",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        // Greek (upper)
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        // Operators / relations
        "times" => "×",
        "cdot" => "·",
        "div" => "÷",
        "pm" => "±",
        "mp" => "∓",
        "leq" | "le" => "≤",
        "geq" | "ge" => "≥",
        "neq" | "ne" => "≠",
        "approx" => "≈",
        "equiv" => "≡",
        "propto" => "∝",
        "to" | "rightarrow" => "→",
        "leftarrow" => "←",
        "leftrightarrow" => "↔",
        "Rightarrow" => "⇒",
        "Leftarrow" => "⇐",
        "Leftrightarrow" => "⇔",
        "in" => "∈",
        "notin" => "∉",
        "subset" => "⊂",
        "supset" => "⊃",
        "subseteq" => "⊆",
        "supseteq" => "⊇",
        "cup" => "∪",
        "cap" => "∩",
        "forall" => "∀",
        "exists" => "∃",
        "neg" | "lnot" => "¬",
        "wedge" | "land" => "∧",
        "vee" | "lor" => "∨",
        // Big operators / misc
        "sum" => "∑",
        "prod" => "∏",
        "int" => "∫",
        "iint" => "∬",
        "oint" => "∮",
        "partial" => "∂",
        "nabla" => "∇",
        "infty" => "∞",
        "sqrt" => "√",
        "angle" => "∠",
        "degree" | "deg" => "°",
        "prime" => "′",
        "ldots" | "dots" => "…",
        "cdots" => "⋯",
        "hbar" => "ℏ",
        "ell" => "ℓ",
        "aleph" => "ℵ",
        "therefore" => "∴",
        "because" => "∵",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_greek_and_operators() {
        assert_eq!(latex_to_unicode(r"\alpha + \beta"), "α + β");
        assert_eq!(latex_to_unicode(r"a \leq b"), "a ≤ b");
        assert_eq!(latex_to_unicode(r"x \times y"), "x × y");
    }

    #[test]
    fn converts_superscripts_and_subscripts() {
        assert_eq!(latex_to_unicode("x^2"), "x²");
        assert_eq!(latex_to_unicode("x^{10}"), "x¹⁰");
        assert_eq!(latex_to_unicode("a_1"), "a₁");
    }

    #[test]
    fn unknown_superscript_is_kept() {
        // 'q' has no unicode superscript: fall back to ^q.
        assert_eq!(latex_to_unicode("y^q"), "y^q");
        assert_eq!(latex_to_unicode("y^{qq}"), "y^(qq)");
    }

    #[test]
    fn keeps_unknown_control_words() {
        assert_eq!(latex_to_unicode(r"\foo bar"), "foo bar");
    }

    #[test]
    fn sqrt_and_braces() {
        assert_eq!(latex_to_unicode(r"\sqrt{x}"), "√x");
        assert_eq!(latex_to_unicode(r"{a+b}"), "a+b");
    }

    #[test]
    fn escaped_chars() {
        assert_eq!(latex_to_unicode(r"100\%"), "100%");
    }
}
