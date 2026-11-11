// The one product-name store lives in @rightkit/document-core (`configureDocumentHost`); this module
// re-exports it so a host sets the name once for every document package.
export { hostAppName, setHostAppName } from "@rightkit/document-core/host";
