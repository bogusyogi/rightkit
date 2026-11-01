import { NOW, QA_ACCOUNT, QA_PDF_B64, QA_PNG_B64, QA_XLSX_B64 } from "./qaMockData";
import type { InvokeArgs } from "./qaMockTypes";

export function qaWorkspaceInvoke(command: string, args?: InvokeArgs): { handled: boolean; value: unknown } {
  let value: unknown;
  switch (command) {
    case "list_calendars":
      value = [{
        id: "primary",
        summary: "Adrian",
        background_color: "#c27b4a",
        primary: true,
        access_role: "owner",
      }];
      break;
    case "list_events":
      value = [];
      break;
    case "create_event":
      value = {
        id: "qa-event",
        calendar_id: String(args?.calendarId ?? "primary"),
        summary: "QA event",
        description: "",
        location: "",
        start: new Date(NOW).toISOString(),
        end: new Date(NOW + 1000 * 60 * 30).toISOString(),
        all_day: false,
        html_link: "https://calendar.google.com/calendar/event?eid=qa",
        meet_link: "",
        organizer: QA_ACCOUNT.display_name,
        attendees: [],
        my_response: "accepted",
        is_recurring: false,
        recurring_event_id: "",
      };
      break;
    case "free_busy":
      value = [];
      break;
    case "list_task_lists":
      value = [{ id: "default", title: "My Tasks" }];
      break;
    case "list_tasks":
      value = [];
      break;
    case "list_drive_files":
      value = {
        files: [
          {
            id: "qa-drive-receipt",
            name: "Receipt-2673-1369.pdf",
            mime_type: "application/pdf",
            modified_time: new Date(NOW - 1000 * 60 * 30).toISOString(),
            size: "48213",
            web_view_link: "https://drive.google.com/file/d/qa-drive-receipt/view",
            icon_link: "",
            thumbnail_link: "",
            is_folder: false,
            parents: [],
          },
        ],
        next_page_token: "",
      };
      break;
    case "drive_storage_quota":
      value = {
        limit: "15000000000",
        usage: "4200000000",
        usage_in_drive: "4200000000",
      };
      break;
    case "drive_attach_file":
      value = {
        filename: String(args?.filename ?? "Receipt-2673-1369.pdf"),
        mimeType: String(args?.mimeType ?? "application/pdf"),
        dataB64: "",
      };
      break;
    case "drive_upload":
      value = {
        id: "qa-uploaded-file",
        name: String(args?.filename ?? "Uploaded QA file.txt"),
        mime_type: String(args?.mimeType ?? "text/plain"),
        modified_time: new Date(NOW).toISOString(),
        size: "42",
        web_view_link: "https://drive.google.com/file/d/qa-uploaded-file/view",
        icon_link: "",
        thumbnail_link: "",
        is_folder: false,
        parents: [],
      };
      break;
    case "drive_download":
      value = "qa://download/Receipt-2673-1369.pdf";
      break;
    case "download_attachment":
      value = `qa://download/${String(args?.filename ?? "attachment.bin")}`;
      break;
    case "preview_attachment":
      if (String(args?.filename ?? "").toLowerCase().endsWith(".png")) {
        value = {
          filename: String(args?.filename ?? "邮件2.0.png"),
          mime_type: "application/octet-stream",
          data_b64: QA_PNG_B64,
        };
        break;
      }
      if (String(args?.filename ?? "").toLowerCase().endsWith(".xlsx")) {
        value = {
          filename: String(args?.filename ?? "orders_export_2026-06-02.xlsx"),
          mime_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
          data_b64: QA_XLSX_B64,
        };
        break;
      }
      value = {
        filename: String(args?.filename ?? "Receipt-2673-1369.pdf"),
        mime_type: String(args?.mimeType ?? "application/pdf"),
        data_b64: QA_PDF_B64,
      };
      break;
    case "scan_file":
      value = "clean";
      break;
    case "save_attachment_to_drive":
      value = {
        id: "qa-saved-attachment",
        name: String(args?.filename ?? "Receipt-2673-1369.pdf"),
        mime_type: String(args?.mimeType ?? "application/pdf"),
        modified_time: new Date(NOW).toISOString(),
        size: "29892",
        web_view_link: "https://drive.google.com/file/d/qa-saved-attachment/view",
        icon_link: "",
        thumbnail_link: "",
        is_folder: false,
        parents: [],
      };
      break;
    case "drive_create_folder":
    case "drive_rename":
    case "drive_move":
      value = {
        id: "qa-drive-folder",
        name: String(args?.folderName ?? args?.name ?? args?.newName ?? "QA Folder"),
        mime_type: "application/vnd.google-apps.folder",
        modified_time: new Date(NOW).toISOString(),
        size: "0",
        web_view_link: "https://drive.google.com/drive/folders/qa-drive-folder",
        icon_link: "",
        thumbnail_link: "",
        is_folder: true,
        parents: [],
      };
      break;
    case "drive_trash":
    case "drive_revoke_permission":
      value = undefined;
      break;
    case "drive_share":
      value = "https://drive.google.com/file/d/qa-drive-receipt/view";
      break;
    case "drive_list_permissions":
      value = [];
      break;
    case "list_all_contacts":
      value = [
        {
          resource_name: "people/qa-contact-1",
          etag: "qa-etag",
          display_name: "Info Damned Designs",
          given_name: "Info",
          family_name: "Damned Designs",
          emails: ["info@damneddesigns.com"],
          phones: [],
          org: "Damned Designs",
        },
      ];
      break;
    case "lookup_contact":
      value = null;
      break;
    case "search_recipients":
      value = [
        {
          email: "info@damneddesigns.com",
          name: "Info Damned Designs",
          source: "contacts",
        },
      ];
      break;
    case "warm_people_cache":
    case "create_contact":
    case "update_contact":
    case "delete_contact":
      value = undefined;
      break;
    case "list_contact_groups":
      value = [];
      break;
    case "expand_contact_group":
      value = [];
      break;
    case "create_meet":
      value = {
        name: "qa-meet",
        meeting_uri: "https://meet.google.com/qa-mail-right",
        meeting_code: "qa-mail-right",
      };
      break;
    default:
      return { handled: false, value: undefined };
  }
  return { handled: true, value };
}
