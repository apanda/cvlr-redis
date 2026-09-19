//! An explicit abstract state machine for Redis's user-visible behavior.
//!
//! Every function cites the redis/src line range it transcribes. When the C and this model
//! disagree, the C wins and the model is a bug -- EXCEPT where a `SUSPECTED DEFECT` note
//! says the divergence is deliberate.
//!
//! What is proven about this module is proven about a MODEL, not about Redis. The
//! correspondence argument lives in `crate::driver` (differential testing against a real
//! redis-server), not in the prover.

pub mod cmd;
pub mod expire;
pub mod state;
pub mod step;
pub mod watch;
