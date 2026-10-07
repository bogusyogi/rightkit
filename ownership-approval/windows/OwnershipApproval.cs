// RightKit ownership approval Windows host helper.
// Signing is available only from this interactive Windows Forms application.
// The private key stays in the Microsoft Platform Crypto Provider; no CLI or
// environment-variable path can invoke it.
using System;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;
using System.Windows.Forms;
using Windows.Security.Credentials.UI;

internal static class OwnershipApproval
{
    private const string KeyName = "RightKit Ownership Approval (Adrian)";
    private const string Host = "windows";
    private static readonly CngProvider Provider = CngProvider.MicrosoftPlatformCryptoProvider;

    [STAThread]
    private static void Main()
    {
        ApplicationConfiguration.Initialize();
        Application.Run(new ApprovalForm());
    }

    private sealed class ApprovalForm : Form
    {
        private readonly Button _load = new() { Text = "Review approval draft…", AutoSize = true };
        private readonly Button _enroll = new() { Text = "Enroll this Windows host…", AutoSize = true };
        private readonly Button _export = new() { Text = "Export enrolled public key…", AutoSize = true, Enabled = false };
        private readonly Button _sign = new() { Text = "Sign reviewed draft…", AutoSize = true, Enabled = false };
        private readonly Label _status = new() { AutoSize = true, Text = "No key or draft loaded." };
        private readonly TextBox _record = new()
        {
            Multiline = true,
            ReadOnly = true,
            ScrollBars = ScrollBars.Both,
            WordWrap = false,
            Dock = DockStyle.Fill,
            Font = new System.Drawing.Font("Consolas", 10)
        };
        private JsonNode? _draft;
        private string? _draftPath;
        private string? _keyId;

        public ApprovalForm()
        {
            Text = "RightKit ownership approval — Windows";
            StartPosition = FormStartPosition.CenterScreen;
            MinimumSize = new System.Drawing.Size(760, 540);
            Size = new System.Drawing.Size(1000, 760);

            var banner = new Label
            {
                AutoSize = true,
                MaximumSize = new System.Drawing.Size(930, 0),
                Text = "This host can sign only after you open and review an unsigned draft here. " +
                       "The signing key is held by the Microsoft Platform Crypto Provider and its per-use OS prompt is required. " +
                       "Enrollment exports only a public key; it does not edit protected verifier trust."
            };
            var actions = new FlowLayoutPanel { Dock = DockStyle.Top, AutoSize = true, WrapContents = true };
            actions.Controls.AddRange(new Control[] { _load, _enroll, _export, _sign });
            var top = new FlowLayoutPanel { Dock = DockStyle.Top, AutoSize = true, FlowDirection = FlowDirection.TopDown, WrapContents = false, Padding = new Padding(10) };
            top.Controls.Add(banner);
            top.Controls.Add(actions);
            top.Controls.Add(_status);
            Controls.Add(_record);
            Controls.Add(top);

            _load.Click += (_, _) => LoadDraft();
            _enroll.Click += async (_, _) => await EnrollAsync();
            _export.Click += (_, _) => ExportPublicKey();
            _sign.Click += async (_, _) => await SignAsync();
            Shown += (_, _) => RefreshKeyState();
        }

        private void RefreshKeyState()
        {
            try
            {
                using var key = OpenApprovalKey();
                using var ecdsa = new ECDsaCng(key);
                _keyId = KeyId(ecdsa);
                _enroll.Enabled = false;
                _export.Enabled = true;
                _status.Text = $"Persistent Windows host key available: {_keyId}.";
            }
            catch (Exception e)
            {
                _keyId = null;
                _enroll.Enabled = true;
                _export.Enabled = false;
                _status.Text = "No usable enrolled key: " + e.Message;
            }
            UpdateSignAvailability();
        }

        private void LoadDraft()
        {
            using var picker = new OpenFileDialog
            {
                Title = "Select an unsigned RightKit ownership approval draft",
                Filter = "Approval draft (*.json)|*.json|JSON files (*.json)|*.json|All files (*.*)|*.*",
                CheckFileExists = true,
                Multiselect = false
            };
            if (picker.ShowDialog(this) != DialogResult.OK) return;
            try
            {
                var text = File.ReadAllText(picker.FileName, new UTF8Encoding(false, true));
                var draft = JsonNode.Parse(text) ?? throw new InvalidDataException("Draft is empty.");
                ValidateDraft(draft);
                _draft = draft;
                _draftPath = Path.GetFullPath(picker.FileName);
                _record.Text = JsonSerializer.Serialize(draft["payload"], PrettyJson);
                _status.Text = "Unsigned payload loaded. Review every field and the canonical payload digest below before signing.";
                _record.Text = "SHA-256 canonical payload: " + PayloadDigest(draft["payload"]!) + Environment.NewLine +
                               "Signing key: " + (_keyId ?? "not enrolled") + Environment.NewLine + Environment.NewLine + _record.Text;
            }
            catch (Exception e)
            {
                _draft = null;
                _draftPath = null;
                _record.Clear();
                ShowError("Draft cannot be reviewed", e.Message);
            }
            UpdateSignAvailability();
        }

        private async System.Threading.Tasks.Task EnrollAsync()
        {
            if (!await VerifyCurrentUserAsync("Enroll this Windows host's RightKit approval key.")) return;
            using var key = CreateApprovalKey();
            using var ecdsa = new ECDsaCng(key);
            RequireApprovedKey(key);
            var keyId = KeyId(ecdsa);
            _keyId = keyId;
            RefreshKeyState();
            ExportPublicKey();
        }

        private void ExportPublicKey()
        {
            using var key = OpenApprovalKey();
            using var ecdsa = new ECDsaCng(key);
            var keyId = KeyId(ecdsa);
            var pem = ecdsa.ExportSubjectPublicKeyInfoPem();
            using var save = new SaveFileDialog
            {
                Title = "Export public key for protected Windows verifier enrollment",
                Filter = "Public key enrollment data (*.public.json)|*.public.json|JSON files (*.json)|*.json",
                FileName = keyId + ".public.json",
                OverwritePrompt = true
            };
            if (save.ShowDialog(this) != DialogResult.OK)
            {
                _status.Text = $"Host key {keyId} is stored locally. Public-key export remains available; protected verifier trust has not changed.";
                MessageBox.Show(this, "The key remains in the Windows key store. You can export its public key from the main window; private key material is never exported.",
                    "Public key export skipped", MessageBoxButtons.OK, MessageBoxIcon.Information);
                return;
            }
            var enrollment = new JsonObject { ["host"] = Host, ["keyId"] = keyId, ["publicKeyPem"] = pem };
            File.WriteAllText(save.FileName, JsonSerializer.Serialize(enrollment, PrettyJson) + Environment.NewLine, new UTF8Encoding(false));
            _keyId = keyId;
            _enroll.Enabled = false;
            _status.Text = $"Public key exported: {keyId}. Protected verifier trust still requires separate administrator provisioning.";
            UpdateSignAvailability();
        }

        private async System.Threading.Tasks.Task SignAsync()
        {
            if (_draft is null || _draftPath is null) return;
            try
            {
                // Re-read immediately before signing to reject a draft changed after display.
                var current = JsonNode.Parse(File.ReadAllText(_draftPath, new UTF8Encoding(false, true)))
                              ?? throw new InvalidDataException("Draft is empty.");
                ValidateDraft(current);
                var currentCanonical = CanonicalJson(current["payload"]!);
                if (!StringComparer.Ordinal.Equals(currentCanonical, CanonicalJson(_draft["payload"]!)))
                    throw new InvalidDataException("Draft payload changed after review. Load it again before signing.");

                using var save = new SaveFileDialog
                {
                    Title = "Save signed RightKit ownership record",
                    Filter = "Signed approval (*.signed.json)|*.signed.json|JSON files (*.json)|*.json",
                    FileName = Path.GetFileNameWithoutExtension(_draftPath) + ".signed.json",
                    InitialDirectory = Path.GetDirectoryName(_draftPath),
                    OverwritePrompt = true
                };
                if (save.ShowDialog(this) != DialogResult.OK) return;
                var digestText = PayloadDigest(current["payload"]!);
                var confirmation = MessageBox.Show(this,
                    "Review the full payload in the main window. This operation signs its canonical JSON bytes.\n\n" +
                    "Repository: " + current["payload"]!["repositoryIdentity"] + "\n" +
                    "Fingerprint: " + current["payload"]!["fingerprint"] + "\n" +
                    "Paths: " + string.Join(", ", current["payload"]!["paths"]!.AsArray().Select(x => x!.GetValue<string>())) + "\n" +
                    "Expires: " + current["payload"]!["expires"] + "\n" +
                    "Canonical SHA-256: " + digestText + "\n" +
                    "Output: " + save.FileName + "\n\n" +
                    "Continue to the Windows key-protection prompt?",
                    "Confirm exact RightKit ownership approval", MessageBoxButtons.YesNo, MessageBoxIcon.Warning, MessageBoxDefaultButton.Button2);
                if (confirmation != DialogResult.Yes) return;

                var verificationMessage = "Sign this RightKit approval only if it matches the review window.\n\n" +
                                          "Canonical payload SHA-256: " + digestText + "\n\n" +
                                          "Signed payload fields:\n" + JsonSerializer.Serialize(current["payload"], PrettyJson);
                if (!await VerifyCurrentUserAsync(verificationMessage)) return;

                using var key = OpenApprovalKey();
                RequireApprovedKey(key);
                using var ecdsa = new ECDsaCng(key);
                var signature = ecdsa.SignHash(Convert.FromHexString(digestText), DSASignatureFormat.Rfc3279DerSequence);
                var signed = current.DeepClone().AsObject();
                signed["signature"] = new JsonObject
                {
                    ["keyId"] = KeyId(ecdsa),
                    ["host"] = Host,
                    ["value"] = Convert.ToBase64String(signature)
                };
                File.WriteAllText(save.FileName, JsonSerializer.Serialize(signed, PrettyJson) + Environment.NewLine, new UTF8Encoding(false));
                _status.Text = "Signed record saved after native key-provider authorization: " + save.FileName;
                MessageBox.Show(this, "Signed approval saved. Protected trust enrollment remains separate.",
                    "Approval signed", MessageBoxButtons.OK, MessageBoxIcon.Information);
            }
            catch (Exception e)
            {
                ShowError("Signing did not complete", e.Message);
            }
        }

        private void UpdateSignAvailability() => _sign.Enabled = _draft is not null && _keyId is not null;

        private async System.Threading.Tasks.Task<bool> VerifyCurrentUserAsync(string reason)
        {
            _sign.Enabled = false;
            _enroll.Enabled = false;
            try
            {
                var result = await UserConsentVerifierInterop.RequestVerificationForWindowAsync(Handle, reason);
                RefreshKeyState();
                if (result == UserConsentVerificationResult.Verified) return true;
                _status.Text = "Windows user verification did not succeed (" + result + "). No key operation was performed.";
                return false;
            }
            catch
            {
                RefreshKeyState();
                throw;
            }
        }
    }

    private static readonly JsonSerializerOptions PrettyJson = new() { WriteIndented = true };

    private static CngKey CreateApprovalKey()
    {
        if (CngKey.Exists(KeyName, Provider))
            throw new InvalidOperationException("A host key with this name already exists. This workflow will not replace or rotate it.");
        var parameters = new CngKeyCreationParameters
        {
            Provider = Provider,
            KeyUsage = CngKeyUsages.Signing,
            ExportPolicy = CngExportPolicies.None,
            KeyCreationOptions = CngKeyCreationOptions.None,
            UIPolicy = ApprovalUiPolicy()
        };
        var key = CngKey.Create(CngAlgorithm.ECDsaP256, KeyName, parameters);
        try { RequireApprovedKey(key); return key; }
        catch { key.Dispose(); throw; }
    }

    private static CngKey OpenApprovalKey()
    {
        var key = CngKey.Open(KeyName, Provider, CngKeyOpenOptions.None);
        try { RequireApprovedKey(key); return key; }
        catch { key.Dispose(); throw; }
    }

    private static CngUIPolicy ApprovalUiPolicy() => new(
        CngUIProtectionLevels.ForceHighProtection,
        "RightKit ownership approval",
        "Authenticate to authorize Adrian's RightKit ownership approval key.",
        "Sign the reviewed RightKit ownership exception.",
        "RightKit ownership approval");

    private static void RequireApprovedKey(CngKey key)
    {
        if (key.Provider?.Provider != Provider.Provider)
            throw new CryptographicException("The signing key is not held by the Microsoft Platform Crypto Provider.");
        if (key.ExportPolicy != CngExportPolicies.None)
            throw new CryptographicException("The signing key permits export; refusing to use it.");
        var policy = key.UIPolicy;
        if (policy is null || policy.ProtectionLevel != CngUIProtectionLevels.ForceHighProtection)
            throw new CryptographicException("The signing key does not require a native provider prompt on every private-key operation.");
    }

    private static void ValidateDraft(JsonNode draft)
    {
        var root = draft.AsObject();
        if (!root.Select(p => p.Key).ToHashSet(StringComparer.Ordinal).SetEquals(new[] { "schema", "payload", "signature" }))
            throw new InvalidDataException("Draft must contain exactly schema, payload, and signature fields.");
        if (root["schema"]?.GetValue<int>() != 1) throw new InvalidDataException("Unsupported approval schema.");
        if (root["signature"] is not null) throw new InvalidDataException("Only unsigned drafts may be loaded.");
        if (root["payload"] is not JsonObject payload) throw new InvalidDataException("Draft payload is missing.");
        var payloadKeys = new[] { "repositoryIdentity", "catalogDigest", "fingerprint", "paths", "occurrenceHashes", "reason", "owner", "expires", "requestId", "trackingRef" };
        if (!payload.Select(p => p.Key).ToHashSet(StringComparer.Ordinal).SetEquals(payloadKeys))
            throw new InvalidDataException("Draft payload must contain exactly the complete schema-1 fields.");
        RequireText(payload, "repositoryIdentity", 512);
        RequireHex(payload, "catalogDigest");
        RequireText(payload, "fingerprint", 512);
        RequireText(payload, "reason", 8192);
        RequireText(payload, "owner", 512);
        RequireText(payload, "expires", 10);
        if (!DateTime.TryParseExact(payload["expires"]!.GetValue<string>(), "yyyy-MM-dd", CultureInfo.InvariantCulture, DateTimeStyles.None, out var expiry) ||
            expiry.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture) != payload["expires"]!.GetValue<string>())
            throw new InvalidDataException("Expiry must be a valid YYYY-MM-DD date.");
        RequireText(payload, "requestId", 128);
        if (!Regex.IsMatch(payload["requestId"]!.GetValue<string>(), "^[A-Za-z0-9][A-Za-z0-9._:-]{2,127}$", RegexOptions.CultureInvariant))
            throw new InvalidDataException("Request ID has an unsupported format.");
        if (payload["paths"] is not JsonArray paths || paths.Count == 0) throw new InvalidDataException("At least one exact repository path is required.");
        var pathValues = paths.Select(x => x?.GetValue<string>() ?? throw new InvalidDataException("Paths must be strings.")).ToArray();
        if (pathValues.Any(p => p.Length == 0 || p.StartsWith('/') || p.Contains('\\') || p.Split('/').Any(s => s is "" or "." or "..") || Regex.IsMatch(p, "[*?\\[\\]{}]", RegexOptions.CultureInvariant)))
            throw new InvalidDataException("Paths must be exact repository-relative paths without traversal or glob syntax.");
        if (pathValues.Distinct(StringComparer.Ordinal).Count() != pathValues.Length) throw new InvalidDataException("Paths contain duplicates.");
        if (payload["occurrenceHashes"] is not JsonArray hashes || hashes.Count == 0) throw new InvalidDataException("Occurrence hashes are required.");
        var hashValues = hashes.Select(x => x?.GetValue<string>() ?? throw new InvalidDataException("Occurrence hashes must be strings.")).ToArray();
        if (hashValues.Any(x => !Regex.IsMatch(x, "^[a-f0-9]{64}$", RegexOptions.CultureInvariant)) || hashValues.Distinct(StringComparer.Ordinal).Count() != hashValues.Length)
            throw new InvalidDataException("Occurrence hashes must be unique lowercase SHA-256 values.");
        if (payload["trackingRef"] is not JsonObject tracking || !tracking.Select(p => p.Key).ToHashSet(StringComparer.Ordinal).SetEquals(new[] { "url", "id", "verified" }) ||
            tracking["url"]?.GetValue<string>() is not string url || !url.StartsWith("https://", StringComparison.Ordinal) ||
            tracking["id"]?.GetValue<string>() is not { Length: > 0 } || tracking["verified"]?.GetValue<bool>() != true)
            throw new InvalidDataException("Tracking reference must be verified, nonempty, and HTTPS-backed.");
    }

    private static void RequireText(JsonObject obj, string key, int maxLength)
    {
        if (obj[key] is not JsonValue value || !value.TryGetValue<string>(out var text) || string.IsNullOrWhiteSpace(text) || text.Length > maxLength)
            throw new InvalidDataException($"Missing or invalid {key}.");
    }

    private static void RequireHex(JsonObject obj, string key)
    {
        RequireText(obj, key, 64);
        if (!Regex.IsMatch(obj[key]!.GetValue<string>(), "^[a-f0-9]{64}$", RegexOptions.CultureInvariant))
            throw new InvalidDataException($"{key} must be a lowercase SHA-256 value.");
    }

    // Match JSON.stringify for the scalar types in approval records: preserve JS-compatible
    // UTF-8 and escape only quotes, backslashes, and control characters.
    private static string CanonicalJson(JsonNode node)
    {
        if (node is JsonObject obj)
            return "{" + string.Join(",", obj.Select(p => p.Key).OrderBy(k => k, StringComparer.Ordinal)
                .Select(k => Quote(k) + ":" + CanonicalJson(obj[k]!))) + "}";
        if (node is JsonArray array) return "[" + string.Join(",", array.Select(x => CanonicalJson(x!))) + "]";
        if (node is JsonValue value)
        {
            if (value.TryGetValue<string>(out var text)) return Quote(text);
            if (value.TryGetValue<bool>(out var boolean)) return boolean ? "true" : "false";
            if (value.TryGetValue<int>(out var integer)) return integer.ToString(CultureInfo.InvariantCulture);
            if (value.TryGetValue<long>(out var longInteger)) return longInteger.ToString(CultureInfo.InvariantCulture);
            if (value.TryGetValue<double>(out var number))
            {
                if (!double.IsFinite(number)) throw new InvalidDataException("Non-finite JSON number is unsupported.");
                return number.ToString("R", CultureInfo.InvariantCulture).Replace("E+", "e+").Replace("E-", "e-").Replace('E', 'e');
            }
        }
        throw new InvalidDataException("Unsupported JSON value in approval payload.");
    }

    private static string Quote(string value)
    {
        var b = new StringBuilder(value.Length + 2).Append('"');
        for (var i = 0; i < value.Length; i++)
        {
            var c = value[i];
            if (char.IsHighSurrogate(c))
            {
                if (i + 1 < value.Length && char.IsLowSurrogate(value[i + 1]))
                {
                    b.Append(c).Append(value[++i]);
                    continue;
                }
                b.Append("\\u").Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
                continue;
            }
            if (char.IsLowSurrogate(c))
            {
                b.Append("\\u").Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
                continue;
            }
            switch (c)
            {
                case '"': b.Append("\\\""); break;
                case '\\': b.Append("\\\\"); break;
                case '\b': b.Append("\\b"); break;
                case '\f': b.Append("\\f"); break;
                case '\n': b.Append("\\n"); break;
                case '\r': b.Append("\\r"); break;
                case '\t': b.Append("\\t"); break;
                default:
                    if (c < 0x20) b.Append("\\u").Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
                    else b.Append(c);
                    break;
            }
        }
        return b.Append('"').ToString();
    }

    private static string PayloadDigest(JsonNode payload) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(CanonicalJson(payload)))).ToLowerInvariant();
    private static string KeyId(ECDsa ecdsa) => "win-" + Convert.ToHexString(SHA256.HashData(ecdsa.ExportSubjectPublicKeyInfo())).ToLowerInvariant();

    private static void ShowError(string title, string message) => MessageBox.Show(title + ":\n\n" + message, title, MessageBoxButtons.OK, MessageBoxIcon.Error);
}
