//! `temzdf print-types`: the TypeScript declarations of the scripting API,
//! `ts-types/temzdf-all.d.ts`. scripts/ts-types.ts writes that file from
//! the declarations written by hand (`ts-types/temzdf-core.d.ts`,
//! `ts-types/temzdf-actions.d.ts`) and the shipped modules' TypeScript, and
//! `cargo xtask check` checks that the committed one is up to date.

/// The declarations, as `temzdf print-types` prints them.
pub const DECLARATIONS: &str = include_str!("../ts-types/temzdf-all.d.ts");
