#![feature(type_alias_impl_trait)]

mod rate_limiter;
mod token_bucket;

pub use crate::{
    rate_limiter::{RateLimitedAsyncRead, SleepingRateLimiter},
    token_bucket::TokenBucket,
};
