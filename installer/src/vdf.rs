// Parser for Steam's text KeyValues format.
/// A parsed text KeyValues node. Keys are matched case-insensitively, as Steam does.
#[derive(Debug, Clone)]
pub enum Kv {
    Str(String),
    Obj(Vec<(String, Kv)>),
}

impl Kv {
    pub fn get(&self, key: &str) -> Option<&Kv> {
        match self {
            Kv::Obj(entries) => entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Kv::Str(_) => None,
        }
    }

    pub fn path(&self, keys: &[&str]) -> Option<&Kv> {
        keys.iter().try_fold(self, |node, key| node.get(key))
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Kv::Str(s) => Some(s),
            Kv::Obj(_) => None,
        }
    }

    pub fn entries(&self) -> &[(String, Kv)] {
        match self {
            Kv::Obj(entries) => entries,
            Kv::Str(_) => &[],
        }
    }
}

/// Parses a text KeyValues document into its root object.
pub fn parse(text: &str) -> Kv {
    let mut tokens = Tokens {
        chars: text.chars().peekable(),
    };
    Kv::Obj(parse_object(&mut tokens))
}

fn parse_object(tokens: &mut Tokens) -> Vec<(String, Kv)> {
    let mut entries = Vec::new();
    while let Some(token) = tokens.next() {
        let key = match token {
            Token::Close => break,
            Token::Open => continue,
            Token::Str(s) => s,
        };
        match tokens.next() {
            Some(Token::Open) => entries.push((key, Kv::Obj(parse_object(tokens)))),
            Some(Token::Str(value)) => entries.push((key, Kv::Str(value))),
            Some(Token::Close) | None => break,
        }
    }
    entries
}

enum Token {
    Open,
    Close,
    Str(String),
}

struct Tokens<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Iterator for Tokens<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        loop {
            let c = *self.chars.peek()?;
            if c.is_whitespace() {
                self.chars.next();
            } else if c == '/' {
                // line comment
                while self.chars.next().is_some_and(|c| c != '\n') {}
            } else if c == '[' {
                // platform conditional like [$WIN32]: ignored
                while self.chars.next().is_some_and(|c| c != ']') {}
            } else {
                break;
            }
        }
        match self.chars.next()? {
            '{' => Some(Token::Open),
            '}' => Some(Token::Close),
            '"' => {
                let mut s = String::new();
                while let Some(c) = self.chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match self.chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => break,
                        },
                        c => s.push(c),
                    }
                }
                Some(Token::Str(s))
            }
            first => {
                let mut s = String::from(first);
                while let Some(&c) = self.chars.peek() {
                    if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                        break;
                    }
                    s.push(c);
                    self.chars.next();
                }
                Some(Token::Str(s))
            }
        }
    }
}
