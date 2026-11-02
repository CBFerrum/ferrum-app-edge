//! M-Bus (EN 13757-3), as heat, water and gas meters answer: a long frame of records, each saying what
//! it measures and in what. Pure, so that a sample can be tried at a desk.

use serde_json::{Map, Value, json};

/// Asks a meter at a primary address for its data.
pub fn request(address: u8) -> [u8; 5] {
    [0x10, 0x5b, address, 0x5b_u8.wrapping_add(address), 0x16]
}

/// What a VIF says: a name, and the power of ten and the unit its number is in.
fn unit(vif: u8) -> Option<(&'static str, i32, &'static str)> {
    let n = i32::from(vif & 0x07);
    let s = i32::from(vif & 0x03);
    Some(match vif & 0x7f {
        0x00..=0x07 => ("energy", n - 3, "Wh"),
        0x08..=0x0f => ("energy", n, "J"),
        0x10..=0x17 => ("volume", n - 6, "m3"),
        0x18..=0x1f => ("mass", n - 3, "kg"),
        0x28..=0x2f => ("power", n - 3, "W"),
        0x30..=0x37 => ("power", n, "J/h"),
        0x38..=0x3f => ("volume.flow", n - 6, "m3/h"),
        0x40..=0x47 => ("volume.flow", n - 7, "m3/min"),
        0x48..=0x4f => ("volume.flow", n - 9, "m3/s"),
        0x50..=0x57 => ("mass.flow", n - 3, "kg/h"),
        0x58..=0x5b => ("temperature.flow", s - 3, "C"),
        0x5c..=0x5f => ("temperature.return", s - 3, "C"),
        0x60..=0x63 => ("temperature.difference", s - 3, "K"),
        0x64..=0x67 => ("temperature.external", s - 3, "C"),
        0x68..=0x6b => ("pressure", s - 3, "bar"),
        _ => return None,
    })
}

fn bcd(b: &[u8]) -> Option<f64> {
    let mut n = 0f64;
    for byte in b.iter().rev() {
        let (high, low) = (byte >> 4, byte & 0xf);
        if high > 9 || low > 9 {
            return None;
        }
        n = n * 100.0 + f64::from(high * 10 + low);
    }
    Some(n)
}

fn integer(b: &[u8]) -> f64 {
    let negative = b.last().is_some_and(|l| l & 0x80 != 0);
    let n = b.iter().rev().fold(if negative { -1i64 } else { 0 }, |n, c| n << 8 | i64::from(*c));
    n as f64
}

/// The data of a long frame, as `{name: {value, unit}}`, with `.1` and so on for stored values and
/// `.tariff1` for tariffs, because a meter says those as well. `None` is not a meter's frame at all.
pub fn decode(frame: &[u8]) -> Option<Value> {
    let (&[0x68, length, again, 0x68], rest) = (frame.get(..4).and_then(|h| <&[u8; 4]>::try_from(h).ok())?, frame.get(4..)?) else { return None };
    let length = usize::from(length);
    if usize::from(again) != length || rest.len() < length + 2 || rest[length + 1] != 0x16 {
        return None;
    }
    if rest[..length].iter().fold(0u8, |s, b| s.wrapping_add(*b)) != rest[length] {
        return None;
    }
    let user = &rest[..length];
    // Control, address, CI, then identification, manufacturer, version, medium, access, status, signature.
    if user.len() < 15 || !matches!(user[2], 0x72 | 0x76) {
        return None;
    }
    let mut out = Map::new();
    out.insert("identification".into(), json!(bcd(&user[3..7])? as u64));
    out.insert("status".into(), json!(user[13]));
    let mut at = 15;
    while at < user.len() {
        let dif = user[at];
        at += 1;
        if dif == 0x2f {
            continue;
        }
        if matches!(dif, 0x0f | 0x1f) {
            break;
        }
        let mut storage = u32::from(dif >> 6 & 1);
        let (mut tariff, mut shift, mut more) = (0u32, 1u32, dif & 0x80 != 0);
        let mut storage_shift = 1;
        while more {
            let dife = *user.get(at)?;
            at += 1;
            storage |= u32::from(dife & 0x0f) << storage_shift;
            storage_shift += 4;
            tariff |= u32::from(dife >> 4 & 3) * shift;
            shift *= 4;
            more = dife & 0x80 != 0;
        }
        let vif = *user.get(at)?;
        at += 1;
        let mut extended = vif & 0x80 != 0;
        while extended {
            extended = *user.get(at)? & 0x80 != 0;
            at += 1;
        }
        let size = match dif & 0x0f {
            0 | 8 => 0,
            1 | 9 => 1,
            2 | 10 => 2,
            3 | 11 => 3,
            4 | 5 | 12 => 4,
            6 | 14 => 6,
            7 => 8,
            13 => {
                let n = usize::from(*user.get(at)?);
                at += 1;
                n
            }
            _ => 0,
        };
        let data = user.get(at..at + size)?;
        at += size;
        let number = match dif & 0x0f {
            1..=4 | 6 | 7 => Some(integer(data)),
            5 => Some(f64::from(f32::from_le_bytes(data.try_into().ok()?))),
            9..=12 | 14 => bcd(data),
            _ => None,
        };
        let (Some(number), Some((name, power, unit))) = (number, unit(vif)) else { continue };
        let mut key = name.to_string();
        if storage > 0 {
            key.push_str(&format!(".{storage}"));
        }
        if tariff > 0 {
            key.push_str(&format!(".tariff{tariff}"));
        }
        let scaled = number * 10f64.powi(power);
        out.insert(key, json!({"value": (scaled * 1e9).round() / 1e9, "unit": unit}));
    }
    Some(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A long frame around `records`, as a meter would send it.
    fn frame(records: &[u8]) -> Vec<u8> {
        let mut user = vec![0x08, 0x05, 0x72];
        user.extend([0x78, 0x56, 0x34, 0x12, 0x24, 0x40, 1, 7, 0x55, 0, 0, 0]);
        user.extend(records);
        let length = user.len() as u8;
        let mut out = vec![0x68, length, length, 0x68];
        out.extend(&user);
        out.push(user.iter().fold(0u8, |s, b| s.wrapping_add(*b)));
        out.push(0x16);
        out
    }

    #[test]
    fn a_meter_says_what_it_measures_and_in_what() {
        let f = frame(&[
            0x04, 0x03, 0x39, 0x30, 0, 0, // 12345 Wh, as an integer
            0x0c, 0x13, 0x78, 0x56, 0x34, 0x12, // 12345678 × 10^-3 m3, as BCD
            0x02, 0x5a, 0xd2, 0x04, // 1234 × 0.1 C, flow
            0x44, 0x03, 0x01, 0, 0, 0, // a stored value
        ]);
        let got = decode(&f).unwrap();
        assert_eq!(got["identification"], json!(12345678));
        assert_eq!(got["energy"], json!({"value": 12345.0, "unit": "Wh"}));
        assert_eq!(got["volume"]["value"], json!(12345.678));
        assert_eq!(got["temperature.flow"], json!({"value": 123.4, "unit": "C"}));
        assert_eq!(got["energy.1"]["value"], json!(1.0));
    }

    #[test]
    fn a_frame_that_does_not_add_up_is_not_believed() {
        let mut f = frame(&[0x04, 0x03, 1, 0, 0, 0]);
        let at = f.len() - 3;
        f[at] ^= 1;
        assert_eq!(decode(&f), None);
        assert_eq!(decode(&[0x68]), None);
        assert_eq!(decode(b"hello there, world"), None);
    }

    #[test]
    fn a_short_request_adds_up() {
        assert_eq!(request(5), [0x10, 0x5b, 5, 0x60, 0x16]);
    }

    #[test]
    fn numbers_as_a_meter_keeps_them() {
        assert_eq!(bcd(&[0x34, 0x12]), Some(1234.0));
        assert_eq!(bcd(&[0xff]), None);
        assert_eq!(integer(&[0xff, 0xff]), -1.0);
    }
}
