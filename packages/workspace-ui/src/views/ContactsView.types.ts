export type Editor = {
  resource_name: string; // "" = new contact
  etag: string;
  name: string;
  emails: string[];
  phones: string[];
};

export const EMPTY_EDITOR: Editor = {
  resource_name: "",
  etag: "",
  name: "",
  emails: [""],
  phones: [""],
};
