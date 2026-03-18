pub(crate) use super::registry::SpawnReservation;
pub(crate) use super::registry::exceeds_thread_spawn_depth_limit;
pub(crate) use super::registry::next_thread_spawn_depth;

pub(crate) type Guards = super::registry::AgentRegistry;
