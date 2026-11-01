import type { RuntimeTab } from "../RuntimePane";

export function runtimeLabel(tab: RuntimeTab) {
  switch (tab) {
    case "context":
      return "Context";
    case "taskgraph":
      return "Task graph";
    case "taste":
      return "Taste";
    case "metrics":
      return "Metrics";
    case "rollouts":
      return "Rollouts";
    case "finetuning":
      return "Fine-tuning";
    case "proposals":
      return "Proposals";
    case "compare":
      return "Compare";
    case "audit":
      return "Audit";
    case "room":
      return "Review Room";
    case "messages":
      return "Messages";
    case "observability":
      return "Observability";
    case "harness":
      return "Harness";
    case "specs":
      return "Specs";
    case "recipes":
      return "Recipes";
    case "triggers":
      return "Webhook triggers";
    case "automations":
      return "Automations";
    case "inbox":
      return "Review Inbox";
    default:
      return tab[0].toUpperCase() + tab.slice(1);
  }
}
