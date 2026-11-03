#[cfg(test)]
mod tests {
    #![allow(clippy::await_holding_lock)]
    use rightkit_llm::agent_providers::KeyStoreBackend;
    include!("tests/part_01.rs");
    include!("tests/part_02.rs");
}
