//! Behavioural suites of the protocol core (design section 7 map, S2 rows):
//! T-FRAME, T-SESS, T-SIZE, T-ERR, T-BUDGET and T-WORK at message level, all
//! in-process against fake streaming transports and the fake signer.

mod budget;
mod errors;
mod frame;
mod harness;
mod session;
mod size;
mod work;
