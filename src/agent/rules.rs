use std::ops::{Add, Sub};
use std::hash::{Hash, Hasher};
use std::iter::{repeat};
use std::collections::{HashMap, BTreeMap};
use std::collections::VecDeque;

use num::traits::ToPrimitive;
use regex::Regex;
use rustc_serialize::json::{Json, ToJson};
use rustc_serialize::{Decodable, Decoder, Encodable, Encoder};

use super::http;
use super::stats::Stats;
use history::{Key};
use history::{History};
use history::{HistoryChunk};
use history::{ValueSet};


pub struct Error(pub &'static str);

impl From<Error> for Box<http::Error> {
    fn from(err: Error) -> Box<http::Error> {
        return Box::new(http::BadRequest(err.0));
    }
}


#[derive(RustcEncodable, RustcDecodable, Debug, Clone, Copy)]
#[derive(PartialEq, Eq, Hash)]
pub enum Source {
    Tip,
    Fine,
    Coarse,
}

#[derive(RustcEncodable, RustcDecodable, Debug, Clone, Copy)]
#[derive(PartialEq, Eq, Hash)]
pub enum Aggregation {
    None,
    CasualSum,  // does ignore absent data points, treating them as zero
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    Eq(String, String),
    NotEq(String, String),
    RegexLike(String, Regex),
    And(Box<Condition>, Box<Condition>),
    Or(Box<Condition>, Box<Condition>),
    Not(Box<Condition>),
    Has(String),
}

impl Hash for Condition {
    fn hash<H>(&self, s: &mut H) where H: Hasher {
        use self::Condition::*;
        match self {
            &Eq(ref a, ref b) => { "eq".hash(s); a.hash(s); b.hash(s); },
            &NotEq(ref a, ref b) => { "not-eq".hash(s); a.hash(s); b.hash(s); },
            &RegexLike(ref a, ref b) => {
                "regex-like".hash(s);
                a.hash(s);
                b.as_str().hash(s);
            },
            &And(ref a, ref b) => { "and".hash(s); a.hash(s); b.hash(s); }
            &Or(ref a, ref b) => { "or".hash(s); a.hash(s); b.hash(s); }
            &Not(ref a) => { "not".hash(s); a.hash(s); }
            &Has(ref a) => { "has".hash(s); a.hash(s); }
        }
    }
}

impl Decodable for Condition {
    fn decode<D: Decoder>(d: &mut D) -> Result<Condition, D::Error> {
        use self::Condition::*;
        d.read_seq(|d, len| {
            let s = try!(d.read_str());
            let norm_len = if s == "not" || s == "has" { 2 } else { 3 };
            if norm_len != len {
                return Err(d.error("Bad tuple length condition"));
            }
            match &s[..] {
                "not" => Ok(Not(Box::new(try!(Decodable::decode(d))))),
                "has" => Ok(Has(try!(Decodable::decode(d)))),
                "and" => Ok(And(
                    Box::new(try!(Decodable::decode(d))),
                    Box::new(try!(Decodable::decode(d))),
                    )),
                "or" => Ok(Or(
                    Box::new(try!(Decodable::decode(d))),
                    Box::new(try!(Decodable::decode(d))),
                    )),
                "eq" => Ok(Eq(try!(d.read_str()), try!(d.read_str()))),
                "not-eq" => Ok(NotEq(try!(d.read_str()), try!(d.read_str()))),
                "regex-like" => Ok(RegexLike(
                    try!(d.read_str()),
                    try!(Regex::new(&try!(d.read_str())[..])
                         .map_err(|_| d.error("Error compiling regex"))))),
                _ => Err(d.error("Bad condition type")),
            }
        })
    }
}

impl Encodable for Condition {
    fn encode<E: Encoder>(&self, e: &mut E) -> Result<(), E::Error> {
        use self::Condition::*;
        let len = match self {
            &Not(_) => 1,
            &Has(_) => 1,
            _ => 2,
        };
        e.emit_seq(len, |e| {
            try!(e.emit_seq_elt(0, |e| (match self {
                &Eq(_, _) => "eq",
                &NotEq(_, _) => "not-eq",
                &RegexLike(_, _) => "regex-like",
                &And(_, _) => "and",
                &Or(_, _) => "or",
                &Not(_) => "not",
                &Has(_) => "has",
            }).encode(e)));
            try!(e.emit_seq_elt(1, |e| match self {
                &Eq(ref x, _) => x.encode(e),
                &NotEq(ref x, _) => x.encode(e),
                &RegexLike(ref x, _) => x.encode(e),
                &And(ref x, _) => x.encode(e),
                &Or(ref x, _) => x.encode(e),
                &Not(ref x) => x.encode(e),
                &Has(ref x) => x.encode(e),
            }));
            if len >= 2 {
                try!(e.emit_seq_elt(2, |e| match self {
                    &Eq(_, ref x) => x.encode(e),
                    &NotEq(_, ref x) => x.encode(e),
                    &RegexLike(_, ref x) => x.as_str().encode(e),
                    &And(_, ref x) => x.encode(e),
                    &Or(_, ref x) => x.encode(e),
                    _ => unreachable!(),
                }));
            }
            Ok(())
        })
    }
}

#[derive(RustcDecodable, RustcEncodable, Debug, Clone, PartialEq, Eq, Hash)]
pub struct RawRule {
    pub source: Source,
    pub condition: Condition,
    pub key: Vec<String>,
}

#[derive(RustcDecodable, RustcEncodable, Debug, Clone)]
pub struct RawQuery {
    pub rules: Vec<RawRule>,
    pub limit: usize,
}

#[derive(RustcDecodable, RustcEncodable, Debug, Clone)]
pub struct RawResult {
    pub fine_metrics: Vec<(BTreeMap<String, String>, HistoryChunk)>,
    pub fine_timestamps: Vec<(u64, u32)>,
}

#[derive(RustcDecodable, Debug, Clone)]
pub struct Rule {
    pub source: Source,
    pub condition: Condition,
    pub key: Vec<String>,
    pub aggregation: Aggregation,
    pub limit: u32,
}

#[derive(RustcDecodable, Debug, Clone)]
pub struct Query {
    pub rules: HashMap<String, Rule>,
}

pub fn match_cond(key: &Key, cond: &Condition) -> bool {
    use self::Condition::*;
    match cond {
        &Eq(ref name, ref value) => key.get(name) == Some(value),
        &NotEq(ref name, ref value) => key.get(name) != Some(value),
        &RegexLike(ref name, ref regex)
        => regex.is_match(key.get(name).unwrap_or(&String::from(""))),
        &And(ref a, ref b) => match_cond(key, a) && match_cond(key, b),
        &Or(ref a, ref b) => match_cond(key, a) || match_cond(key, b),
        &Not(ref x) => !match_cond(key, x),
        &Has(ref name) => key.get(name).is_some(),
    }
}

fn query_tip(_rule: &Rule, _stats: &Stats) -> Result<Json, Error> {
    unimplemented!()
}

fn tree_insert(map: &mut BTreeMap<String, Json>, key: &[&str], val: Json) {
    if key.len() == 1 {
        map.insert(key[0].to_string(), val);
    } else {
        tree_insert(
            map.entry(key[0].to_string())
                .or_insert_with(|| Json::Object(BTreeMap::new()))
                .as_object_mut().unwrap(),
            &key[1..], val);
    }
}

// TODO(tailhook) move it to some utitity module?
// TODO(tailhook) or maybe get rid of this crappy dict tree?
pub fn json_tree<'x, I>(iter: I) -> Json
    where I: Iterator<Item=(Vec<&'x str>, Json)>
{
    let mut res = BTreeMap::new();
    for (key, val) in iter {
        if key.len() == 0 {
            // assert!(iter.next().is_none());
            return val;
        }
        tree_insert(&mut res, &key[..], val);
    }
    return Json::Object(res);
}

pub fn take_raw<T:ToJson, I:Iterator<Item=Option<T>>>(items: Vec<I>, limit: u32)
    -> Json
{
    items.into_iter()
        .map(|x| x.take(limit as usize).collect::<Vec<_>>())
        .collect::<Vec<_>>().to_json()
}

pub fn take_rate<T, I>(ts: &VecDeque<(u64, u32)>, items: Vec<I>, limit: u32)
    -> Json
    where I: Iterator<Item=Option<T>> + Clone,
          T: Sub<T, Output=T>+ToPrimitive
{
    items.into_iter().map(|iter| {
        let iter = iter.zip(ts);
        let mut pair = iter.clone();
        pair.next();
        iter.zip(pair)
            .map(|((new, nts), (old, ots))| new
                .and_then(|new| old.and_then(|old| (new - old).to_f64()))
                .map(|r| r * 1000. / (nts.0 - ots.0) as f64))
            .take(limit as usize)
            .collect::<Vec<Option<f64>>>()
    }).collect::<Vec<_>>().to_json()
}

pub fn sum_raw<T, I>(items: Vec<I>, limit: u32, zero: T) -> Json
    where T: Add<T, Output=T> + ToJson + Copy,
          I: Iterator<Item=Option<T>>
{
    let buf: Vec<_> = items.into_iter()
        .map(|x| x.take(limit as usize)
                  .map(|x| x.unwrap_or(zero))
                  .collect::<Vec<_>>())
        .collect();
    let rlen = buf.iter().map(Vec::len).max().unwrap_or(0);
    let zeros = repeat(zero).take(rlen).collect::<Vec<T>>();
    buf.into_iter()
        .fold(zeros, |mut acc, val| {
            for (i, v) in val.into_iter().enumerate() {
                acc[i] = acc[i] + v;
            }
            acc
        })
        .to_json()
}

pub fn sum_rate<T, I>(ts: &VecDeque<(u64, u32)>, items: Vec<I>, limit: u32)
    -> Json
    where I: Iterator<Item=Option<T>> + Clone,
          T: Sub<T, Output=T> + ToPrimitive + Copy
{
    let buf: Vec<_> = items.into_iter().map(|iter| {
        let iter = iter.zip(ts);
        let mut pair = iter.clone();
        pair.next();
        iter.zip(pair)
            .map(|((new, nts), (old, ots))| new
                .and_then(|new| old.and_then(|old| (new - old).to_f64()))
                .map(|r| r * 1000. / (nts.0 - ots.0) as f64)
                .unwrap_or(0.))
            .take(limit as usize)
            .collect::<Vec<f64>>()
    }).collect();
    let rlen = buf.iter().map(Vec::len).max().unwrap_or(0);
    let zeros = repeat(0.).take(rlen).collect::<Vec<_>>();
    buf.into_iter()
        .fold(zeros, |mut acc, val| {
            for (i, v) in val.into_iter().enumerate() {
                acc[i] = acc[i] + v;
            }
            acc
        })
        .to_json()
}

fn query_fine(rule: &Rule, stats: &Stats) -> Result<Json, Error> {
    use self::Aggregation::*;
    let mut keys = HashMap::<_, ValueSet>::new();
    for (ref key, value) in stats.history.fine.iter() {
        if match_cond(key, &rule.condition) {
            let target_key = rule.key.iter()
                             .map(|x| key.get(x).unwrap_or(""))
                             .collect::<Vec<_>>();
            keys.entry(target_key)
                .or_insert_with(ValueSet::Empty)
                .push(value);
        }
    }
    Ok(json_tree(keys.into_iter().map(|(key, stream)| {
        use history::ValueSet::*;
        let ts = &stats.history.fine_timestamps;
        let json = match rule.aggregation {
            None => stream.map(|s| match s {
                Empty => Json::Null,
                Counters(x) => take_rate(ts, x, rule.limit),
                Integers(x) => take_raw(x, rule.limit),
                Floats(x) => take_raw(x, rule.limit),
            }),
            CasualSum => stream.map(|s| match s {
                Empty => Json::Null,
                Counters(x) => sum_rate(ts, x, rule.limit),
                Integers(x) => sum_raw(x, rule.limit, 0),
                Floats(x) => sum_raw(x, rule.limit, 0.),
            }),
        };
        (key, json.unwrap_or(Json::Null))
    })))
}

// Get rid of Result, it always succeed
pub fn query(query: &Query, stats: &Stats) -> Result<Json, Error> {
    let mut items = BTreeMap::new();
    for (name, rule) in query.rules.iter() {
        match rule.source {
            Source::Tip => {
                items.insert(name.clone(), try!(query_tip(rule, stats)));
            }
            Source::Fine => {
                items.insert(name.clone(), try!(query_fine(rule, stats)));
            }
            Source::Coarse => unimplemented!(),
        }
    }
    return Ok(Json::Object(items));
}

pub fn query_raw<'x, I:Iterator<Item=&'x RawRule>>(
    rules: I, limit: usize, history: &History)
    -> RawResult
{
    let mut fine_metrics = Vec::new();
    for rule in rules {
        match rule.source {
            Source::Tip => unimplemented!(),
            Source::Fine => {
                for key in history.fine.keys() {
                    if !match_cond(key, &rule.condition) {
                        continue;
                    }
                    fine_metrics.push((key.get_map(),
                        history.get_fine_history(key)
                            .unwrap().take(limit)));
                }
            }
            Source::Coarse => unimplemented!(),
        }
    }
    RawResult {
        fine_metrics: fine_metrics,
        fine_timestamps: history.fine_timestamps.iter()
            .take(limit).cloned().collect(),
    }
}
