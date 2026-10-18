import Foundation

// RightKitEngine is the native-backed product: SupervisedEngine, NativeBridge and the shutdown
// types. It re-exports the client-only product, so every existing `import RightKitEngine` still
// sees EngineClient, AttachedEngine and the value types with no source change.
@_exported import RightKitEngineClient
