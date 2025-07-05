#[derive(Debug)]
pub enum BatchOp {
    Put(String, String),
    Del(String),
}