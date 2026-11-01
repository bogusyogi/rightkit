#if os(iOS)
import Darwin

// Retain the shared mobile library ABI so older saved sessions fail clearly.
// iOS inference is supplied exclusively by configured cloud providers.
@_cdecl("coderight_ios_foundation_models_infer")
func coderightIOSFoundationModelsInfer(_ request: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    strdup(#"{"error":"Apple Intelligence has been removed. Choose a configured cloud provider."}"#)
}

@_cdecl("coderight_ios_foundation_models_free_string")
func coderightIOSFoundationModelsFreeString(_ value: UnsafeMutablePointer<CChar>?) {
    free(value)
}
#endif
