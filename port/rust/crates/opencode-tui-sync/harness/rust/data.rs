//! The data-layer cases (`createData`): filled in with the store milestone.

use std::rc::Rc;

use opencode_protocol::Json;

use crate::env::{obj, Script};

pub async fn run(_spec: &Json, _script: &Rc<Script>) -> Json {
    obj(vec![("crashed", Json::String("data cases are not implemented yet".into()))])
}
