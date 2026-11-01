// Example worker entry: one site's configuration. Deploy with `wrangler deploy` using wrangler.toml.
import { createWaitlistWorker } from "@rightkit/waitlist-worker";

export default createWaitlistWorker({
  allowedOrigins: ["https://example.com", "https://www.example.com"],
  detailFields: [
    { name: "current_tool", maxLength: 80 },
    { name: "biggest_frustration", maxLength: 500 },
  ],
  email: {
    from: "Example <hello@example.com>",
    replyTo: "hello@example.com",
    subject: "You're on the waitlist",
    text: "Thanks for joining the waitlist. We will email you when the download is ready.",
    html: "<p>Thanks for joining the waitlist. We will email you when the download is ready.</p>",
  },
});
