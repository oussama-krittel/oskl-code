//! Emmet-style abbreviations, expanded on Tab: `View.card>Text*2` becomes a View with two Texts.
//!
//! Supported: names (`div`, `View`, `Animated.View`), `.class`, `#id`, `[attr=value]`,
//! `{text}`, `>` child, `+` sibling, `^` up a level, `*N` repeat with `$` numbering, and
//! `( )` groups. In React Native files `.card` becomes `style={styles.card}` and `#id`
//! becomes `testID`; in other JSX `className`; in HTML `class`.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Html,
    Jsx,
    ReactNative,
}

#[derive(Clone, Debug, Default)]
struct Node {
    /// Empty: the implicit element (div / View / li…).
    name: String,
    group: bool,
    classes: Vec<String>,
    id: Option<String>,
    attrs: Vec<(String, Option<String>)>,
    text: Option<String>,
    children: Vec<Node>,
    count: usize,
}

const HTML_TAGS: &[&str] = &[
    "a", "abbr", "address", "article", "aside", "audio", "b", "blockquote", "body", "br", "button", "canvas", "caption", "code", "col", "dd", "details", "dialog", "div", "dl", "dt", "em", "fieldset", "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "head", "header", "hr", "html", "i", "iframe", "img", "input", "label", "legend", "li", "link", "main", "meta", "nav", "ol", "optgroup", "option", "p", "picture", "pre", "section", "select", "small", "source", "span", "strong", "sub", "summary", "sup", "svg", "table", "tbody", "td", "textarea", "tfoot", "th", "thead", "time", "title", "tr", "u", "ul", "video",
];
const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
/// React Native components that are written self-closing.
const RN_SELF_CLOSING: &[&str] = &["Image", "TextInput", "ActivityIndicator", "Switch", "StatusBar", "FlatList", "SectionList", "RefreshControl"];

/// Expands `abbr` written at a line indented by `base_indent`. Returns the text and where the
/// cursor goes in it (char offset: the first empty spot). None when it isn't an abbreviation
/// worth expanding (plain words and unknown lowercase names are left alone, so Tab indents).
pub fn expand(abbr: &str, target: Target, indent_unit: &str, base_indent: &str) -> Option<(String, usize)> {
    let chars: Vec<char> = abbr.chars().collect();
    if chars.is_empty() || chars.len() > 300 {
        return None;
    }
    let mut p = Parser { s: &chars, i: 0 };
    let (nodes, _) = p.list(0)?;
    if p.i != chars.len() || nodes.is_empty() {
        return None;
    }
    if !nodes.iter().all(|n| valid(n, target)) {
        return None;
    }
    // A lone one-letter word in a script (`i`, `a`) is more likely a variable than a tag.
    let plain = !abbr.contains(['.', '#', '>', '+', '*', '[', '{', '^', '(']);
    if plain && target == Target::Jsx && chars.len() < 2 {
        return None;
    }
    let mut r = Render { target, unit: indent_unit, base: base_indent, out: String::new(), cursor: None };
    for (k, n) in nodes.iter().enumerate() {
        if k > 0 {
            r.newline(0);
        }
        r.node(n, 0, None, "");
    }
    let cursor = r.cursor.unwrap_or(r.out.chars().count());
    Some((r.out, cursor))
}

/// Where (char index) the abbreviation ending at the end of `before` (the line up to the
/// cursor) starts: after the last space, `;`, `,`, `=` or unmatched bracket outside `[]`,
/// `{}` and quotes.
pub fn abbreviation_start(before: &str) -> usize {
    let chars: Vec<char> = before.chars().collect();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut i = chars.len();
    while i > 0 {
        let c = chars[i - 1];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            i -= 1;
            continue;
        }
        match c {
            '"' | '\'' if depth > 0 => quote = Some(c),
            ']' | '}' | ')' => depth += 1,
            '[' | '{' | '(' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            c if depth == 0 && (c.is_whitespace() || matches!(c, ';' | ',' | '=' | '<' | '`' | '"' | '\'' | '!' | '?' | '&' | '|')) => break,
            _ => {}
        }
        i -= 1;
    }
    i
}

fn known(name: &str, target: Target) -> bool {
    if name.is_empty() {
        return true;
    }
    let first_upper = name.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    match target {
        Target::ReactNative => first_upper,
        Target::Jsx => first_upper || HTML_TAGS.contains(&name),
        Target::Html => HTML_TAGS.contains(&name.to_ascii_lowercase().as_str()) || name.contains('-'),
    }
}

/// Every element name is a tag or component (not a variable like `styles.card`).
fn valid(n: &Node, target: Target) -> bool {
    (n.group || known(&n.name, target)) && n.children.iter().all(|c| valid(c, target))
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    /// Siblings at one level, and how many levels a `^` asks to climb after them.
    fn list(&mut self, depth: usize) -> Option<(Vec<Node>, usize)> {
        let mut nodes = Vec::new();
        loop {
            let mut item = self.item(depth)?;
            match self.peek() {
                Some('>') => {
                    self.i += 1;
                    if item.group {
                        return None;
                    }
                    let (children, climb) = self.list(depth + 1)?;
                    item.children = children;
                    nodes.push(item);
                    match climb {
                        0 => return Some((nodes, 0)),
                        1 => continue,
                        n => return Some((nodes, n - 1)),
                    }
                }
                Some('+') => {
                    self.i += 1;
                    nodes.push(item);
                }
                Some('^') => {
                    let mut n = 0;
                    while self.peek() == Some('^') {
                        self.i += 1;
                        n += 1;
                    }
                    nodes.push(item);
                    if depth == 0 {
                        continue;
                    }
                    return Some((nodes, n));
                }
                Some(')') | None => {
                    nodes.push(item);
                    return Some((nodes, 0));
                }
                _ => return None,
            }
        }
    }

    fn item(&mut self, depth: usize) -> Option<Node> {
        let mut node = if self.peek() == Some('(') {
            self.i += 1;
            let (children, _) = self.list(depth)?;
            if self.peek() != Some(')') {
                return None;
            }
            self.i += 1;
            Node { group: true, children, count: 1, ..Default::default() }
        } else {
            self.element()?
        };
        if self.peek() == Some('*') {
            self.i += 1;
            let digits: String = self.s[self.i..].iter().take_while(|c| c.is_ascii_digit()).collect();
            self.i += digits.len();
            node.count = digits.parse().ok().filter(|n| (1..=100).contains(n))?;
        }
        Some(node)
    }

    fn element(&mut self) -> Option<Node> {
        let mut n = Node { count: 1, ..Default::default() };
        // Name, with `.Upper` parts joined on (`Animated.View`).
        if self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            loop {
                while self.peek().is_some_and(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ':')) {
                    n.name.push(self.s[self.i]);
                    self.i += 1;
                }
                if self.peek() == Some('.') && self.s.get(self.i + 1).is_some_and(|c| c.is_ascii_uppercase()) {
                    n.name.push('.');
                    self.i += 1;
                    continue;
                }
                break;
            }
        }
        let mut any = !n.name.is_empty();
        loop {
            match self.peek() {
                Some('.') => {
                    self.i += 1;
                    let w = self.word()?;
                    n.classes.push(w);
                }
                Some('#') => {
                    self.i += 1;
                    n.id = Some(self.word()?);
                }
                Some('[') => {
                    self.i += 1;
                    self.attrs(&mut n)?;
                }
                Some('{') => {
                    self.i += 1;
                    let start = self.i;
                    let mut depth = 1;
                    while let Some(c) = self.peek() {
                        if c == '{' {
                            depth += 1;
                        } else if c == '}' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        self.i += 1;
                    }
                    if self.peek() != Some('}') {
                        return None;
                    }
                    n.text = Some(self.s[start..self.i].iter().collect());
                    self.i += 1;
                }
                _ => break,
            }
            any = true;
        }
        any.then_some(n)
    }

    /// A class or id: letters, digits, `-`, `_` and `$` (numbering), not starting with a digit.
    fn word(&mut self) -> Option<String> {
        let w: String = self.s[self.i..].iter().take_while(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '$')).collect();
        if w.is_empty() || w.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        self.i += w.chars().count();
        Some(w)
    }

    /// `[a=b c="d e" onPress={go} disabled]`, after the `[`.
    fn attrs(&mut self, n: &mut Node) -> Option<()> {
        loop {
            while self.peek() == Some(' ') {
                self.i += 1;
            }
            match self.peek()? {
                ']' => {
                    self.i += 1;
                    return Some(());
                }
                _ => {}
            }
            let name: String = self.s[self.i..].iter().take_while(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '@')).collect();
            if name.is_empty() {
                return None;
            }
            self.i += name.chars().count();
            let value = if self.peek() == Some('=') {
                self.i += 1;
                Some(match self.peek()? {
                    q @ ('"' | '\'') => {
                        self.i += 1;
                        let v: String = self.s[self.i..].iter().take_while(|c| **c != q).collect();
                        self.i += v.chars().count();
                        if self.peek() != Some(q) {
                            return None;
                        }
                        self.i += 1;
                        format!("\"{v}\"")
                    }
                    '{' => {
                        let start = self.i;
                        let mut depth = 0;
                        while let Some(c) = self.peek() {
                            self.i += 1;
                            if c == '{' {
                                depth += 1;
                            } else if c == '}' {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                        }
                        if depth != 0 {
                            return None;
                        }
                        self.s[start..self.i].iter().collect()
                    }
                    _ => {
                        let v: String = self.s[self.i..].iter().take_while(|c| !matches!(c, ' ' | ']')).collect();
                        self.i += v.chars().count();
                        format!("\"{v}\"")
                    }
                })
            } else {
                None
            };
            n.attrs.push((name, value));
        }
    }
}

struct Render<'a> {
    target: Target,
    unit: &'a str,
    base: &'a str,
    out: String,
    cursor: Option<usize>,
}

/// `$` → the repeat number (`$$` pads to two digits).
fn number(s: &str, index: Option<usize>) -> String {
    let Some(i) = index else { return s.to_string() };
    let mut out = String::new();
    let mut run = 0;
    for c in s.chars().chain(std::iter::once('\0')) {
        if c == '$' {
            run += 1;
            continue;
        }
        if run > 0 {
            out.push_str(&format!("{:0width$}", i, width = run));
            run = 0;
        }
        if c != '\0' {
            out.push(c);
        }
    }
    out
}

impl Render<'_> {
    fn newline(&mut self, depth: usize) {
        self.out.push('\n');
        self.out.push_str(self.base);
        for _ in 0..depth {
            self.out.push_str(self.unit);
        }
    }

    fn slot(&mut self) {
        if self.cursor.is_none() {
            self.cursor = Some(self.out.chars().count());
        }
    }

    fn implicit(&self, parent: &str) -> &'static str {
        match (self.target, parent) {
            (Target::ReactNative, _) => "View",
            (_, "ul" | "ol") => "li",
            (_, "table" | "tbody" | "thead") => "tr",
            (_, "tr") => "td",
            (_, "select" | "optgroup") => "option",
            _ => "div",
        }
    }

    fn node(&mut self, n: &Node, depth: usize, index: Option<usize>, parent: &str) {
        for k in 0..n.count {
            if k > 0 {
                self.newline(depth);
            }
            let index = if n.count > 1 { Some(k + 1) } else { index };
            if n.group {
                for (j, c) in n.children.iter().enumerate() {
                    if j > 0 {
                        self.newline(depth);
                    }
                    self.node(c, depth, index, parent);
                }
            } else {
                self.element(n, depth, index, parent);
            }
        }
    }

    fn element(&mut self, n: &Node, depth: usize, index: Option<usize>, parent: &str) {
        let name = if n.name.is_empty() { self.implicit(parent).to_string() } else { n.name.clone() };
        let jsx = self.target != Target::Html;
        self.out.push('<');
        self.out.push_str(&name);
        if let Some(id) = &n.id {
            let key = if self.target == Target::ReactNative { "testID" } else { "id" };
            self.out.push_str(&format!(" {key}=\"{}\"", number(id, index)));
        }
        if !n.classes.is_empty() {
            let classes: Vec<String> = n.classes.iter().map(|c| number(c, index)).collect();
            match self.target {
                Target::ReactNative if classes.len() == 1 => self.out.push_str(&format!(" style={{styles.{}}}", classes[0])),
                Target::ReactNative => self.out.push_str(&format!(" style={{[{}]}}", classes.iter().map(|c| format!("styles.{c}")).collect::<Vec<_>>().join(", "))),
                Target::Jsx => self.out.push_str(&format!(" className=\"{}\"", classes.join(" "))),
                Target::Html => self.out.push_str(&format!(" class=\"{}\"", classes.join(" "))),
            }
        }
        for (k, v) in &n.attrs {
            self.out.push(' ');
            self.out.push_str(k);
            match v {
                Some(v) => {
                    self.out.push('=');
                    self.out.push_str(&number(v, index));
                }
                None if jsx && k.starts_with("on") && k.chars().nth(2).is_some_and(|c| c.is_ascii_uppercase()) => {
                    self.out.push_str("={");
                    self.slot();
                    self.out.push('}');
                }
                None => {
                    self.out.push_str("=\"");
                    self.slot();
                    self.out.push('"');
                }
            }
        }
        let void = VOID.contains(&name.to_ascii_lowercase().as_str()) || (self.target == Target::ReactNative && RN_SELF_CLOSING.contains(&name.as_str()));
        if void && n.children.is_empty() && n.text.is_none() {
            self.out.push_str(if jsx { " />" } else { ">" });
            return;
        }
        self.out.push('>');
        if let Some(t) = &n.text {
            self.out.push_str(&number(t, index));
        }
        if n.children.is_empty() {
            if n.text.is_none() {
                self.slot();
            }
        } else {
            for c in &n.children {
                self.newline(depth + 1);
                self.node(c, depth + 1, index, &name);
            }
            self.newline(depth);
        }
        self.out.push_str("</");
        self.out.push_str(&name);
        self.out.push('>');
    }
}

#[cfg(test)]
mod tests {
    use super::{Target, abbreviation_start, expand};

    fn ex(abbr: &str, t: Target) -> Option<String> {
        expand(abbr, t, "  ", "").map(|(s, c)| {
            let mut s: Vec<char> = s.chars().collect();
            s.insert(c, '|');
            s.into_iter().collect()
        })
    }

    #[test]
    fn react_native_views() {
        assert_eq!(ex("View.card>Text*2", Target::ReactNative).unwrap(), "<View style={styles.card}>\n  <Text>|</Text>\n  <Text></Text>\n</View>");
        assert_eq!(ex("Pressable[onPress]>Text{Go}", Target::ReactNative).unwrap(), "<Pressable onPress={|}>\n  <Text>Go</Text>\n</Pressable>");
        assert_eq!(ex(".row.wide>Image", Target::ReactNative).unwrap(), "<View style={[styles.row, styles.wide]}>\n  <Image />\n</View>|");
        assert_eq!(ex("Animated.View#box", Target::ReactNative).unwrap(), "<Animated.View testID=\"box\">|</Animated.View>");
        assert_eq!(ex("View", Target::ReactNative).unwrap(), "<View>|</View>");
    }

    #[test]
    fn web_and_html() {
        assert_eq!(ex("div.box#main>span{Hi}+img[src=a.png]", Target::Jsx).unwrap(), "<div id=\"main\" className=\"box\">\n  <span>Hi</span>\n  <img src=\"a.png\" />\n</div>|");
        assert_eq!(ex("ul>li.item$*3", Target::Html).unwrap(), "<ul>\n  <li class=\"item1\">|</li>\n  <li class=\"item2\"></li>\n  <li class=\"item3\"></li>\n</ul>");
        assert_eq!(ex("ul>.x", Target::Html).unwrap(), "<ul>\n  <li class=\"x\">|</li>\n</ul>");
        assert_eq!(ex("div>p^span", Target::Html).unwrap(), "<div>\n  <p>|</p>\n</div>\n<span></span>");
        assert_eq!(ex("(dt+dd)*2", Target::Html).unwrap(), "<dt>|</dt>\n<dd></dd>\n<dt></dt>\n<dd></dd>");
    }

    #[test]
    fn leaves_code_alone() {
        for code in ["styles.card", "foo", "x*2", "a>b", "this.props", ".5", "View>", "View[", "user.name"] {
            assert_eq!(ex(code, Target::ReactNative), None, "{code}");
        }
        assert_eq!(ex("foo", Target::Jsx), None);
        assert!(ex("a>b", Target::Jsx).is_some(), "known HTML tags in web JSX");
    }

    #[test]
    fn finds_where_the_abbreviation_starts() {
        let line = "  return (View.card>Text{Hi there}*2";
        assert_eq!(&line[abbreviation_start(line)..], "View.card>Text{Hi there}*2");
        let line = "const a = Pressable[onPress={() => go(1)}]>Text";
        assert_eq!(&line[abbreviation_start(line)..], "Pressable[onPress={() => go(1)}]>Text");
        assert_eq!(abbreviation_start("    "), 4);
    }
}
