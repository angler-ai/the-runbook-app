# Slack

A project can post its approvals, handoffs, reminders and failed runs to a Slack channel. There are two ways to set it up:

| | Incoming webhook | The Runbook's Slack app |
|---|---|---|
| Set up by | a project admin | the server operator once, then project admins |
| Messages | a line linking to the run | the same, plus one button per approval option |
| Voting from Slack | no | yes, as the person who clicks |

## Incoming webhook

In **Project settings → Slack notifications**, paste an incoming-webhook URL (`https://hooks.slack.com/…`). The URL is stored and never shown again.

## The Slack app (approve and reject from Slack)

1. **Create the app** from [`deploy/slack/manifest.yaml`](../deploy/slack/manifest.yaml): at api.slack.com/apps choose **Create New App → From a manifest**. First replace `runbook.example.com` with your server's host name, so Slack sends button clicks to `https://<host>/v1/slack/interactions`. Install the app to your workspace.
2. **Give the server its two secrets**:
   - `RUNBOOK_SLACK_BOT_TOKEN`: the Bot User OAuth Token (`xoxb-…`);
   - `RUNBOOK_SLACK_SIGNING_SECRET`: **Basic Information → Signing Secret**.

   Optionally, set `RUNBOOK_SLACK_DEFAULT_CHANNEL` to a channel ID for projects that don't name one. On Cloudflare, set them as Worker secrets (`wrangler secret put`) on the API Worker. With the GKE Terraform, use `slack_bot_token`, `slack_signing_secret` and `slack_default_channel`.
3. **Pick each project's channel.** Invite the app to the channel (`/invite @The Runbook`). Then, in **Project settings → Slack notifications**, enter the channel's ID (in Slack, the channel's details end with `C…` or `G…`).

### How a click becomes a vote

- Each click is checked against Slack's signature, made with your signing secret. Requests more than five minutes old are refused.
- Only clicks from the workspace the app is installed in count; people from other workspaces in a shared channel are refused.
- The person who clicked is looked up in Slack (`users.info`) and matched to a Runbook account by email. That account must be an active member of the organization.
- The vote is cast through the normal decision route, with that person's own permissions. Approver rules, minimums and one-vote-per-person apply exactly as in the app. The token used exists for one minute and is revoked straight away.
- The person gets a reply only they can see, such as "Your vote (approve) is recorded." or "You are not an approver for this step."

Someone whose Slack email doesn't match a Runbook account is told so, and decides in the app instead.

Slack messages are best effort. Email, webhooks and the app's inbox stay the record, and a Slack outage never holds up a run.
