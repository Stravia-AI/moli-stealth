// The prefix added by BoringSSL's native build and referenced by generated Rust bindings.
// Using the crate name avoids collisions with other TLS implementations in the process.
pub const PREFIX: &str = "btls_sys";

#[derive(Debug)]
pub struct PrefixCallback;

impl bindgen::callbacks::ParseCallbacks for PrefixCallback {
    fn generated_link_name_override(
        &self,
        item_info: bindgen::callbacks::ItemInfo<'_>,
    ) -> Option<String> {
        Some(format!("{PREFIX}_{}", item_info.name))
    }
}
