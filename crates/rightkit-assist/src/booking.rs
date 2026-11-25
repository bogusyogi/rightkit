//! Structured booking extraction: schema.org hotel and flight reservations from the JSON-LD and microdata a
//! reservation mail carries. Dates, places and the reference only; everything is read in code, validated, and
//! length-capped. A block that does not parse, or a reservation without a usable date, is skipped.

use crate::facts::{Booking, BookingKind, LocalDateTime, Place};
use crate::markup::{self, Node, Obj};
use crate::text;
use serde_json::Value;

const MAX_JSON_BYTES: usize = 256 * 1024;
const MAX_JSON_NODES: usize = 4000;
const MAX_VALUES_PER_PROP: usize = 16;
const MAX_PROPS: usize = 64;
const MAX_JSON_DEPTH: usize = 12;
const MAX_WALK_DEPTH: usize = 6;
const MAX_BOOKINGS: usize = 16;

const LODGING_TYPES: [&str; 7] = [
    "Hotel",
    "LodgingBusiness",
    "Resort",
    "Hostel",
    "BedAndBreakfast",
    "Motel",
    "Campground",
];

fn last_segment(raw: &str) -> String {
    raw.rsplit(|c: char| c == '/' || c == '#' || c == ':')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn json_to_node(value: &Value, depth: usize, budget: &mut usize) -> Option<Node> {
    if *budget == 0 || depth > MAX_JSON_DEPTH {
        return None;
    }
    *budget -= 1;
    match value {
        Value::String(s) => Some(Node::Text(text::clean(s, 512))),
        Value::Number(n) => Some(Node::Text(n.to_string())),
        Value::Bool(_) | Value::Null | Value::Array(_) => None,
        Value::Object(map) => {
            let mut obj = Obj::default();
            match map.get("@type") {
                Some(Value::String(t)) => obj.types.push(last_segment(t)),
                Some(Value::Array(ts)) => {
                    for t in ts.iter().take(8) {
                        if let Value::String(t) = t {
                            obj.types.push(last_segment(t));
                        }
                    }
                }
                _ => {}
            }
            for (key, val) in map.iter().take(MAX_PROPS) {
                if key.starts_with('@') {
                    continue;
                }
                match val {
                    Value::Array(values) => {
                        for v in values.iter().take(MAX_VALUES_PER_PROP) {
                            if let Some(node) = json_to_node(v, depth + 1, budget) {
                                obj.props.push((key.clone(), node));
                            }
                        }
                    }
                    other => {
                        if let Some(node) = json_to_node(other, depth + 1, budget) {
                            obj.props.push((key.clone(), node));
                        }
                    }
                }
            }
            Some(Node::Obj(obj))
        }
    }
}

fn json_roots(value: &Value, out: &mut Vec<Node>, budget: &mut usize) {
    match value {
        Value::Array(items) => {
            for item in items.iter().take(MAX_VALUES_PER_PROP * 4) {
                json_roots(item, out, budget);
            }
        }
        Value::Object(map) => {
            if let Some(Value::Array(graph)) = map.get("@graph") {
                for item in graph.iter().take(MAX_VALUES_PER_PROP * 4) {
                    json_roots(item, out, budget);
                }
            }
            if let Some(node) = json_to_node(value, 0, budget) {
                out.push(node);
            }
        }
        _ => {}
    }
}

fn when(obj: &Obj, names: &[&str]) -> Option<LocalDateTime> {
    for name in names {
        for (k, v) in &obj.props {
            if k.eq_ignore_ascii_case(name) {
                if let Node::Text(t) = v {
                    if let Some(parsed) = LocalDateTime::parse(t) {
                        return Some(parsed);
                    }
                }
            }
        }
    }
    None
}

fn city_of(obj: &Obj) -> Option<String> {
    let city = match obj.get("address") {
        Some(Node::Obj(address)) => address.text_of(&["addressLocality"]),
        _ => None,
    };
    city.map(|c| text::clean(&c, 80)).filter(|c| !c.is_empty())
}

fn place_of(parent: &Obj, prop: &str) -> Option<Place> {
    let place = match parent.get(prop)? {
        Node::Obj(o) => Place {
            code: o
                .text_of(&["iataCode"])
                .map(|c| text::clean(&c, 8).to_ascii_uppercase())
                .filter(|c| !c.is_empty()),
            name: o
                .text_of(&["name"])
                .map(|n| text::clean(&n, 80))
                .filter(|n| !n.is_empty()),
            city: city_of(o),
        },
        Node::Text(t) => Place {
            code: None,
            name: Some(text::clean(t, 80)).filter(|n| !n.is_empty()),
            city: None,
        },
    };
    if place.is_empty() {
        None
    } else {
        Some(place)
    }
}

fn reference_of(obj: &Obj) -> Option<String> {
    obj.text_of(&["reservationNumber", "reservationId"])
        .map(|r| text::clean(&r, 40))
        .filter(|r| !r.is_empty())
}

fn hotel_from(reservation: &Obj, subject: Option<&Obj>) -> Option<Booking> {
    let start = when(
        reservation,
        &["checkinTime", "checkinDate", "checkInTime", "checkInDate"],
    );
    let end = when(
        reservation,
        &[
            "checkoutTime",
            "checkoutDate",
            "checkOutTime",
            "checkOutDate",
        ],
    );
    if start.is_none() && end.is_none() {
        return None;
    }
    let place = subject.and_then(|s| {
        let city = city_of(s);
        let place = Place {
            code: None,
            name: None,
            city,
        };
        if place.is_empty() {
            None
        } else {
            Some(place)
        }
    });
    Some(Booking {
        kind: BookingKind::Hotel,
        reference: reference_of(reservation),
        name: subject
            .and_then(|s| s.text_of(&["name"]))
            .map(|n| text::clean(&n, 80))
            .filter(|n| !n.is_empty()),
        start,
        end,
        place,
        from: None,
        to: None,
    })
}

fn flight_from(reservation: &Obj, flight: &Obj) -> Option<Booking> {
    let start = when(flight, &["departureTime"]);
    let end = when(flight, &["arrivalTime"]);
    if start.is_none() && end.is_none() {
        return None;
    }
    let number = flight
        .text_of(&["flightNumber"])
        .map(|n| text::clean(&n, 12))
        .filter(|n| !n.is_empty());
    let carrier = match flight.get("airline") {
        Some(Node::Obj(a)) => a.text_of(&["iataCode", "name"]),
        _ => None,
    }
    .map(|c| text::clean(&c, 20))
    .filter(|c| !c.is_empty());
    let name = match (carrier, number) {
        (Some(c), Some(n)) => Some(format!("{c} {n}")),
        (None, Some(n)) => Some(n),
        (Some(c), None) => Some(c),
        (None, None) => None,
    };
    Some(Booking {
        kind: BookingKind::Flight,
        reference: reference_of(reservation),
        name,
        start,
        end,
        place: None,
        from: place_of(flight, "departureAirport"),
        to: place_of(flight, "arrivalAirport"),
    })
}

fn walk(obj: &Obj, depth: usize, out: &mut Vec<Booking>) {
    if depth > MAX_WALK_DEPTH || out.len() >= MAX_BOOKINGS {
        return;
    }
    let subject = obj.get_obj("reservationFor");
    let lodging_subject = subject
        .map(|s| LODGING_TYPES.iter().any(|t| s.has_type(t)))
        .unwrap_or(false);
    let flight_subject = subject.map(|s| s.has_type("Flight")).unwrap_or(false);
    let booking = if obj.has_type("LodgingReservation")
        || (obj.has_type("Reservation") && lodging_subject)
    {
        hotel_from(obj, subject)
    } else if obj.has_type("FlightReservation") || (obj.has_type("Reservation") && flight_subject) {
        subject.and_then(|f| flight_from(obj, f))
    } else {
        None
    };
    if let Some(b) = booking {
        if !out.contains(&b) {
            out.push(b);
        }
    }
    for (_, v) in &obj.props {
        if let Node::Obj(child) = v {
            walk(child, depth + 1, out);
        }
    }
}

/// Every hotel and flight reservation in a mail's HTML, from JSON-LD first and microdata second, without
/// repeats. At most 16.
pub fn extract(html: &str) -> Vec<Booking> {
    let mut roots: Vec<Node> = Vec::new();
    let mut budget = MAX_JSON_NODES;
    for block in markup::ld_json_blocks(html) {
        if block.len() > MAX_JSON_BYTES {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(block.trim()) {
            json_roots(&value, &mut roots, &mut budget);
        }
    }
    roots.extend(markup::microdata(html));
    let mut out = Vec::new();
    for node in &roots {
        if let Node::Obj(obj) = node {
            walk(obj, 0, &mut out);
        }
    }
    out
}
