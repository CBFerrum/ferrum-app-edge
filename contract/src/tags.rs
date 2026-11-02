//! How a definition names a thing inside equipment, for protocols that read and write by name. These
//! are checked when a definition is saved, so a mistake is a person's to fix at a desk.

/// A value's shape in a PLC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Byte,
    Int,
    Word,
    Dint,
    Dword,
    Real,
    Lreal,
    Text(usize),
}

impl Kind {
    pub fn parse(text: &str) -> Option<Kind> {
        Some(match text.to_lowercase().as_str() {
            "bool" => Kind::Bool,
            "byte" | "usint" | "sint" => Kind::Byte,
            "int" | "i16" => Kind::Int,
            "word" | "uint" | "u16" => Kind::Word,
            "dint" | "i32" => Kind::Dint,
            "dword" | "udint" | "u32" => Kind::Dword,
            "real" | "f32" => Kind::Real,
            "lreal" | "f64" => Kind::Lreal,
            other => Kind::Text(other.strip_prefix("string")?.trim_matches(['(', ')']).parse().ok().filter(|n| (1..=254).contains(n)).unwrap_or(80)),
        })
    }

    pub fn size(self) -> usize {
        match self {
            Kind::Bool | Kind::Byte => 1,
            Kind::Int | Kind::Word => 2,
            Kind::Dint | Kind::Dword | Kind::Real => 4,
            Kind::Lreal => 8,
            Kind::Text(n) => n + 2,
        }
    }
}

/// `DB1.DBD4:real`, `DB1.DBX0.3`, `MW10`, `M0.1`, `IB2`, `QD8:real`. Without a type, the width says:
/// a bit is a bool, a byte a byte, a word an int and a double a dint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct S7Address {
    /// `0x84` a data block, `0x83` markers, `0x81` inputs, `0x82` outputs.
    pub area: u8,
    pub block: u16,
    pub byte: u32,
    pub bit: u8,
    pub kind: Kind,
}

impl S7Address {
    pub fn parse(text: &str) -> Result<S7Address, String> {
        let wrong = || format!("{text} is not an S7 address: try DB1.DBD4:real, MW10 or I0.1");
        let (place, kind) = match text.split_once(':') {
            Some((place, kind)) => (place, Some(Kind::parse(kind).ok_or_else(wrong)?)),
            None => (text, None),
        };
        let upper = place.to_uppercase();
        let (area, block, rest) = match upper.strip_prefix("DB") {
            Some(rest) => {
                let (block, rest) = rest.split_once(".DB").ok_or_else(wrong)?;
                (0x84, block.parse::<u16>().map_err(|_| wrong())?, rest.to_string())
            }
            None => {
                let area = match upper.chars().next() {
                    Some('M') => 0x83,
                    Some('I' | 'E') => 0x81,
                    Some('Q' | 'A') => 0x82,
                    _ => return Err(wrong()),
                };
                (area, 0, upper[1..].to_string())
            }
        };
        let (width, number) = match rest.chars().next() {
            Some(c @ ('X' | 'B' | 'W' | 'D')) => (c, &rest[1..]),
            Some(c) if c.is_ascii_digit() => ('X', rest.as_str()),
            _ => return Err(wrong()),
        };
        let (byte, bit) = number.split_once('.').map_or((number, "0"), |(b, i)| (b, i));
        let (byte, bit) = (byte.parse().map_err(|_| wrong())?, bit.parse::<u8>().ok().filter(|b| *b < 8).ok_or_else(wrong)?);
        let kind = kind.unwrap_or(match width {
            'X' => Kind::Bool,
            'B' => Kind::Byte,
            'W' => Kind::Int,
            _ => Kind::Dint,
        });
        Ok(S7Address { area, block, byte, bit, kind })
    }
}

/// `MAIN.counter:dint`, or `Program:Main.Speed` and the like. The type can be left to what the far end says.
pub fn symbol(text: &str) -> Result<(&str, Option<Kind>), String> {
    let (name, kind) = match text.rsplit_once(':') {
        Some((name, kind)) if Kind::parse(kind).is_some() => (name, Kind::parse(kind)),
        _ => (text, None),
    };
    if name.is_empty() { Err("say which tag".into()) } else { Ok((name, kind)) }
}

/// An object and a property: `analog-input:1/present-value`, or by numbers, `0:1/85`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BacnetAddress {
    pub object_type: u16,
    pub instance: u32,
    pub property: u32,
}

const OBJECT_TYPES: &[(&str, u16)] = &[
    ("analog-input", 0),
    ("analog-output", 1),
    ("analog-value", 2),
    ("binary-input", 3),
    ("binary-output", 4),
    ("binary-value", 5),
    ("device", 8),
    ("multi-state-input", 13),
    ("multi-state-output", 14),
    ("multi-state-value", 19),
];
const PROPERTIES: &[(&str, u32)] =
    &[("present-value", 85), ("object-name", 77), ("description", 28), ("status-flags", 111), ("units", 117), ("reliability", 103)];

impl BacnetAddress {
    pub fn parse(text: &str) -> Result<BacnetAddress, String> {
        let wrong = || format!("{text} is not a BACnet address: try analog-input:1/present-value");
        let (object, property) = text.split_once('/').unwrap_or((text, "present-value"));
        let (kind, instance) = object.split_once(':').ok_or_else(wrong)?;
        let object_type = OBJECT_TYPES.iter().find(|(n, _)| *n == kind).map(|(_, c)| *c).or_else(|| kind.parse().ok()).ok_or_else(wrong)?;
        let property = PROPERTIES.iter().find(|(n, _)| *n == property).map(|(_, c)| *c).or_else(|| property.parse().ok()).ok_or_else(wrong)?;
        Ok(BacnetAddress { object_type, instance: instance.parse().map_err(|_| wrong())?, property })
    }
}

/// `1.3.6.1.2.1.1.3.0`, or `walk:1.3.6.1.2.1.2.2` for everything under it.
pub fn oid(text: &str) -> Result<(Vec<u32>, bool), String> {
    let (walk, dotted) = text.strip_prefix("walk:").map_or((false, text), |rest| (true, rest));
    let arcs = dotted
        .trim_start_matches('.')
        .split('.')
        .map(str::parse)
        .collect::<Result<Vec<u32>, _>>()
        .map_err(|_| format!("{text} is not an OID: try 1.3.6.1.2.1.1.3.0"))?;
    if arcs.len() < 2 || arcs[0] > 2 {
        return Err(format!("{text} is not an OID: try 1.3.6.1.2.1.1.3.0"));
    }
    Ok((arcs, walk))
}

/// Whether a protocol's tag can be read at all, for checking a definition.
pub fn check(scheme: &str, tag: &str) -> Result<(), String> {
    match scheme {
        "s7" => S7Address::parse(tag).map(drop),
        "bacnet" => BacnetAddress::parse(tag).map(drop),
        "snmp" => oid(tag).map(drop),
        "ads" | "enip" => symbol(tag).map(drop),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s7_addresses_read_as_step_7_writes_them() {
        let a = S7Address::parse("DB1.DBD4:real").unwrap();
        assert_eq!((a.area, a.block, a.byte, a.bit, a.kind), (0x84, 1, 4, 0, Kind::Real));
        assert_eq!(S7Address::parse("db12.dbx3.5").unwrap().bit, 5);
        assert_eq!(S7Address::parse("DB1.DBW2").unwrap().kind, Kind::Int);
        assert_eq!(S7Address::parse("MD8").unwrap().kind, Kind::Dint);
        let m = S7Address::parse("M10.2").unwrap();
        assert_eq!((m.area, m.byte, m.bit, m.kind), (0x83, 10, 2, Kind::Bool));
        assert_eq!(S7Address::parse("I0.1").unwrap().area, 0x81);
        assert_eq!(S7Address::parse("QB2").unwrap().area, 0x82);
        assert_eq!(S7Address::parse("DB1.DBB0:string(10)").unwrap().kind, Kind::Text(10));
        for bad in ["", "Z1", "DB1", "DB1.DBZ0", "M0.9", "DBx.DBW0", "DB1.DBW0:wrong"] {
            assert!(S7Address::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn bacnet_objects_go_by_name_or_number() {
        assert_eq!(BacnetAddress::parse("analog-input:1/present-value").unwrap(), BacnetAddress { object_type: 0, instance: 1, property: 85 });
        assert_eq!(BacnetAddress::parse("binary-value:7").unwrap().property, 85, "present value, unless it says otherwise");
        assert_eq!(BacnetAddress::parse("2:3/77").unwrap(), BacnetAddress { object_type: 2, instance: 3, property: 77 });
        assert!(BacnetAddress::parse("analog-input").is_err());
        assert!(BacnetAddress::parse("nonsense:1").is_err());
    }

    #[test]
    fn oids_and_walks() {
        assert_eq!(oid("1.3.6.1").unwrap(), (vec![1, 3, 6, 1], false));
        assert_eq!(oid("walk:.1.3.6").unwrap(), (vec![1, 3, 6], true));
        assert!(oid("1").is_err() && oid("a.b").is_err() && oid("9.1").is_err());
    }

    #[test]
    fn a_symbol_can_say_its_type() {
        assert_eq!(symbol("MAIN.speed:real").unwrap(), ("MAIN.speed", Some(Kind::Real)));
        assert_eq!(symbol("Program:Main.Speed").unwrap(), ("Program:Main.Speed", None), "a colon need not be a type");
        assert!(symbol(":real").is_err());
    }
}
