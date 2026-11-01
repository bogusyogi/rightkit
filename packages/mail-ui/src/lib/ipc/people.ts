import { invoke } from "./invoke";

export interface ContactCard {
  display_name: string;
  email: string;
  photo_url: string;
  org_title: string;
}

export function lookupContact(accountId: string, email: string): Promise<ContactCard | null> {
  return invoke<ContactCard | null>("lookup_contact", { accountId, email });
}

export interface Recipient {
  name: string;
  email: string;
  photo_url: string;
}

export function searchRecipients(accountId: string, query: string): Promise<Recipient[]> {
  return invoke<Recipient[]>("search_recipients", { accountId, query });
}

export function warmPeopleCache(accountId: string): Promise<void> {
  return invoke<void>("warm_people_cache", { accountId });
}

export interface ContactFull {
  resource_name: string;
  etag: string;
  display_name: string;
  given_name: string;
  family_name: string;
  emails: string[];
  phones: string[];
  org: string;
  photo_url: string;
}

export function listAllContacts(accountId: string): Promise<ContactFull[]> {
  return invoke<ContactFull[]>("list_all_contacts", { accountId });
}

export function createContact(
  accountId: string,
  name: string,
  emails: string[],
  phones: string[]
): Promise<void> {
  return invoke<void>("create_contact", { accountId, name, emails, phones });
}

export function updateContact(
  accountId: string,
  resourceName: string,
  etag: string,
  name: string,
  emails: string[],
  phones: string[]
): Promise<void> {
  return invoke<void>("update_contact", { accountId, resourceName, etag, name, emails, phones });
}

export function deleteContact(accountId: string, resourceName: string): Promise<void> {
  return invoke<void>("delete_contact", { accountId, resourceName });
}

export interface ContactGroup {
  resource_name: string;
  name: string;
  member_count: number;
}

export function listContactGroups(accountId: string): Promise<ContactGroup[]> {
  return invoke<ContactGroup[]>("list_contact_groups", { accountId });
}

export function expandContactGroup(accountId: string, resourceName: string): Promise<string[]> {
  return invoke<string[]>("expand_contact_group", { accountId, resourceName });
}
