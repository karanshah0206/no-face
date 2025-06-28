use std::hash::Hasher;

use fnv::FnvHasher;
use minijinja::State;

fn hash(state: &State, tag: &str) -> u64 {
    let name = state.lookup("name").unwrap();
    let name = name.as_str().unwrap();

    let style = state.lookup("style").unwrap();
    let style = style.as_str().unwrap();

    let mut hasher = FnvHasher::with_key(0);
    hasher.write(style.as_bytes());
    hasher.write(name.as_bytes());
    hasher.write(tag.as_bytes());
    hasher.finish()
}

pub fn number(state: &State, tag: String, max: u64) -> u64 {
    let data = hash(state, &tag);
    data % max
}

pub fn color_data(state: &State, tag: String) -> (u64, u64, u64) {
    let mut data = hash(state, &tag);

    let h = data % 360;
    data /= 360;
    let s = data % 20 + 70;
    data /= 20;
    let v = data % 20 + 55;

    (h, s, v)
}

pub fn color(state: &State, tag: String) -> String {
    let (h, s, v) = color_data(state, tag);
    format!("hsl({h}, {s}%, {v}%)")
}

pub fn inverted_color(state: &State, tag: String) -> String {
    let (h, s, v) = color_data(state, tag);
    let h = (h + 180) % 360;
    format!("hsl({h}, {s}%, {v}%)")
}
