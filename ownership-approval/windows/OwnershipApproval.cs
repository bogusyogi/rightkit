// RightKit ownership approval Windows host helper.
// All key operations are explicit, visible Windows Forms commands.
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
using Windows.Security.Credentials;
using Windows.Security.Cryptography;
using Windows.Security.Cryptography.Core;
using Windows.Storage.Streams;
using Windows.Security.Credentials.UI;

internal static class OwnershipApproval
{
    private const string KeyName = "RightKit Ownership Approval (Adrian)";
    private const string KeyLogCredentialName = "rightkit.ownership.keylog.v1";
    private const string Host = "windows";
    private static readonly CngProvider Provider = CngProvider.MicrosoftPlatformCryptoProvider;

    [STAThread]
    private static int Main(string[] args)
    {
        NativeCommand? command = null;
        try { command = NativeCommand.Parse(args); }
        catch (Exception e)
        {
            MessageBox.Show(e.Message, "Invalid native command", MessageBoxButtons.OK, MessageBoxIcon.Error);
            return 2;
        }
        ApplicationConfiguration.Initialize();
        Application.Run(new ApprovalForm(command));
        return Environment.ExitCode;
    }

    private sealed record NativeCommand(string Mode, string OutputPath, string? InputPath = null, string? Name = null)
    {
        public static NativeCommand? Parse(string[] args)
        {
            if (args.Length == 0) return null;
            string mode; string output; string? input = null; string? name = null;
            if (args.Length == 3 && args[0] == "--enroll" && args[1] == "--out") { mode = "enroll"; output = args[2]; }
            else if (args.Length == 3 && args[0] == "--public-key" && args[1] == "--out") { mode = "public"; output = args[2]; }
            else if (args.Length == 4 && args[0] == "--key-operation" && args[2] == "--out") { mode = "operation"; input = args[1]; output = args[3]; }
            else if (args.Length == 5 && args[0] == "--enroll-root" && args[1] == "--name" && args[3] == "--out") { mode = "root"; name = args[2]; output = args[4]; }
            else throw new ArgumentException("Usage: --enroll --out <absolute-public.json> | --public-key --out <absolute-public.json> | --key-operation <absolute-draft.json> --out <absolute-signed.json> | --enroll-root --name <name> --out <absolute-event.json>");
            if (!Path.IsPathFullyQualified(output) || !output.EndsWith(".json", StringComparison.OrdinalIgnoreCase)) throw new ArgumentException("Output path must be absolute and end in .json.");
            if (input is not null && (!Path.IsPathFullyQualified(input) || !input.EndsWith(".json", StringComparison.OrdinalIgnoreCase))) throw new ArgumentException("Draft path must be absolute and end in .json.");
            if (name is not null && (string.IsNullOrWhiteSpace(name) || name.Length > 128)) throw new ArgumentException("Root name must contain 1 to 128 characters.");
            return new NativeCommand(mode, Path.GetFullPath(output), input is null ? null : Path.GetFullPath(input), name);
        }
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
        private KeyCredential? _helloCredential;
        private readonly NativeCommand? _command;
        private bool _commandStarted;
        private JsonObject? _keyLogCommandPayload;
        private string? _keyLogCommandOutput;
        private string? _keyLogCommandDraft;

        public ApprovalForm(NativeCommand? command)
        {
            _command = command;
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
            _export.Click += async (_, _) => await ExportPublicKeyAsync();
            _sign.Click += async (_, _) =>
            {
                if (_keyLogCommandPayload is not null)
                {
                    try { await SignKeyLogCommandAsync(); }
                    catch (Exception e) { ShowError("Key-log signing did not complete", e.Message); Environment.ExitCode = 1; Close(); }
                }
                else await SignAsync();
            };
            Shown += async (_, _) =>
            {
                await RefreshKeyStateAsync();
                if (_command is null || _commandStarted) return;
                _commandStarted = true;
                await RunCommandAsync(_command);
            };
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

        private async System.Threading.Tasks.Task RefreshKeyStateAsync()
        {
            try
            {
                var result = await KeyCredentialManager.OpenAsync(KeyLogCredentialName);
                _helloCredential = result.Status == KeyCredentialStatus.Success ? result.Credential : null;
                if (_helloCredential is not null)
                {
                    _keyId = "win-" + PublicKeyFingerprint(PublicKeyPem(_helloCredential));
                    _enroll.Enabled = false;
                    _export.Enabled = true;
                    _status.Text = "Windows Hello key credential available: " + _keyId + ".";
                    UpdateSignAvailability();
                    return;
                }
                if (result.Status == KeyCredentialStatus.NotFound) { RefreshKeyState(); return; }
                throw new InvalidOperationException("Windows Hello key lookup failed: " + result.Status + ".");
            }
            catch (Exception e)
            {
                _helloCredential = null;
                _keyId = null;
                _enroll.Enabled = false;
                _export.Enabled = false;
                _status.Text = "Windows Hello key lookup failed: " + e.Message;
                UpdateSignAvailability();
            }
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
            using var save = new SaveFileDialog { Title = "Export public key for protected Windows verifier enrollment", Filter = "Public key enrollment data (*.public.json)|*.public.json", FileName = "windows.public.json", OverwritePrompt = true };
            if (save.ShowDialog(this) != DialogResult.OK) return;
            await EnrollToPathAsync(Path.GetFullPath(save.FileName));
        }

        private async System.Threading.Tasks.Task RunCommandAsync(NativeCommand command)
        {
            try
            {
                if (command.Mode is "operation" or "root") Environment.ExitCode = 1;
                EnsureExclusiveOutput(command.OutputPath);
                EnsureOutputDirectory(command.OutputPath);
                switch (command.Mode)
                {
                    case "enroll": await EnrollKeyCredentialAsync(command.OutputPath); break;
                    case "public": await ExportPublicKeyToPathAsync(command.OutputPath); break;
                    case "operation": await StageKeyOperationAsync(command.InputPath!, command.OutputPath); break;
                    case "root": await EnrollRootAsync(command.Name!, command.OutputPath); break;
                }
            }
            catch (Exception e)
            {
                ShowError("Native key command failed", e.Message);
                Environment.ExitCode = 1;
                Close();
            }
        }

        private async System.Threading.Tasks.Task EnrollToPathAsync(string outputPath)
        {
            try
            {
                EnsureExclusiveOutput(outputPath);
                if (CngKey.Exists(KeyName, Provider)) throw new InvalidOperationException("A Windows host key already exists; enrollment will not replace it.");
                if (!await VerifyCurrentUserAsync("Enroll this Windows host's RightKit approval key."))
                    throw new OperationCanceledException("Windows Hello/user verification was canceled or did not succeed.");
                using var key = CreateApprovalKey();
                using var ecdsa = new ECDsaCng(key);
                RequireApprovedKey(key);
                var keyId = KeyId(ecdsa);
                var enrollment = new JsonObject { ["host"] = Host, ["keyId"] = keyId, ["publicKeyPem"] = ecdsa.ExportSubjectPublicKeyInfoPem() };
                var json = JsonSerializer.Serialize(enrollment, PrettyJson) + Environment.NewLine;
                AtomicWriteExclusive(outputPath, json);
                using var checkKey = OpenApprovalKey(); using var checkEcdsa = new ECDsaCng(checkKey);
                var parsed = JsonNode.Parse(File.ReadAllText(outputPath, new UTF8Encoding(false, true)))!.AsObject();
                if (parsed["host"]?.GetValue<string>() != Host || parsed["keyId"]?.GetValue<string>() != KeyId(checkEcdsa) || parsed["publicKeyPem"]?.GetValue<string>() != checkEcdsa.ExportSubjectPublicKeyInfoPem())
                    throw new InvalidDataException("Written public enrollment record did not match the persistent host key.");
                _keyId = keyId;
                _status.Text = "Public enrollment key written: " + outputPath;
                if (_command is not null) { Environment.ExitCode = 0; Close(); }
                else
                    MessageBox.Show(this, "Public key enrollment record saved. Protected verifier trust still requires separate administrator provisioning.", "Enrollment complete", MessageBoxButtons.OK, MessageBoxIcon.Information);
            }
            catch (OperationCanceledException e)
            {
                _status.Text = e.Message;
                if (_command is not null) { Environment.ExitCode = 1; Close(); }
            }
            catch (Exception e)
            {
                ShowError("Enrollment did not complete", e.Message);
                if (_command is not null) { Environment.ExitCode = 1; Close(); }
            }
        }

        private async System.Threading.Tasks.Task EnrollRootAsync(string name, string outputPath)
        {
            await EnsureKeyCredentialNotPresentAsync();
            var result = await KeyCredentialManager.RequestCreateAsync(KeyLogCredentialName, KeyCredentialCreationOption.FailIfExists);
            var credential = RequireCredential(result, "Root key creation");
            var record = KeyRecord(credential, name);
            var keyId = record["keyId"]!.GetValue<string>();
            var payload = new JsonObject
            {
                ["domain"] = "rightkit.ownership-key-log.v1", ["rootFingerprint"] = keyId,
                ["sequence"] = 0, ["previousHash"] = null, ["operation"] = "root",
                ["subject"] = record,
                ["signerKeyId"] = keyId
            };
            ValidateKeyLogPayload(payload, true);
            var genesis = new JsonObject { ["schema"] = 1, ["payload"] = payload, ["signature"] = null };
            AtomicWriteExclusive(outputPath, JsonSerializer.Serialize(genesis, PrettyJson) + Environment.NewLine);
            Environment.ExitCode = 0;
            _record.Text = "Unsigned Windows ownership root genesis event:\r\n" + JsonSerializer.Serialize(genesis, PrettyJson);
            _status.Text = "Root key created with Windows Hello; genesis event saved unsigned for administrator root pinning: " + outputPath;
            Close();
        }

        private async System.Threading.Tasks.Task EnrollKeyCredentialAsync(string outputPath)
        {
            await EnsureKeyCredentialNotPresentAsync();
            var result = await KeyCredentialManager.RequestCreateAsync(KeyLogCredentialName, KeyCredentialCreationOption.FailIfExists);
            var credential = RequireCredential(result, "Windows Hello key enrollment");
            var pem = PublicKeyPem(credential);
            var legacyId = "win-" + PublicKeyFingerprint(pem);
            var enrollment = new JsonObject { ["host"] = Host, ["keyId"] = legacyId, ["publicKeyPem"] = pem };
            AtomicWriteExclusive(outputPath, JsonSerializer.Serialize(enrollment, PrettyJson) + Environment.NewLine);
            Environment.ExitCode = 0;
            _record.Text = JsonSerializer.Serialize(enrollment, PrettyJson);
            _status.Text = "Windows Hello public enrollment record saved: " + outputPath;
            Close();
        }

        private async System.Threading.Tasks.Task ExportPublicKeyToPathAsync(string outputPath)
        {
            var opened = await KeyCredentialManager.OpenAsync(KeyLogCredentialName);
            var credential = RequireCredential(opened, "Open existing Windows Hello key");
            var pem = PublicKeyPem(credential);
            var enrollment = new JsonObject { ["host"] = Host, ["keyId"] = "win-" + PublicKeyFingerprint(pem), ["publicKeyPem"] = pem };
            AtomicWriteExclusive(outputPath, JsonSerializer.Serialize(enrollment, PrettyJson) + Environment.NewLine);
            Environment.ExitCode = 0;
            Close();
        }

        private static KeyCredential RequireCredential(KeyCredentialRetrievalResult result, string action)
        {
            if (result.Status == KeyCredentialStatus.Success && result.Credential is not null) return result.Credential;
            if (result.Status == KeyCredentialStatus.UserCanceled) throw new OperationCanceledException(action + " was canceled in Windows Hello.");
            throw new InvalidOperationException(action + " failed: " + result.Status + ".");
        }

        private static async System.Threading.Tasks.Task EnsureKeyCredentialNotPresentAsync()
        {
            var existing = await KeyCredentialManager.OpenAsync(KeyLogCredentialName);
            if (existing.Status == KeyCredentialStatus.Success) throw new InvalidOperationException("A Windows Hello key-log credential already exists; enrollment will not replace it.");
            if (existing.Status != KeyCredentialStatus.NotFound) throw new InvalidOperationException("Could not verify Windows Hello key state: " + existing.Status + ".");
        }

        private static string PublicKeyPem(KeyCredential credential)
        {
            var der = BufferBytes(credential.RetrievePublicKey(CryptographicPublicKeyBlobType.X509SubjectPublicKeyInfo));
            using var rsa = RSA.Create();
            rsa.ImportSubjectPublicKeyInfo(der, out var consumed);
            if (consumed != der.Length || rsa.KeySize != 2048) throw new CryptographicException("Windows Hello key must be RSA-2048 SubjectPublicKeyInfo.");
            return rsa.ExportSubjectPublicKeyInfoPem();
        }

        private static JsonObject KeyRecord(KeyCredential credential, string deviceName) => new()
        {
            ["keyId"] = PublicKeyFingerprint(PublicKeyPem(credential)),
            ["publicKeyPem"] = PublicKeyPem(credential),
            ["platform"] = Host,
            ["deviceName"] = deviceName
        };

        private static byte[] BufferBytes(IBuffer buffer) => Convert.FromBase64String(CryptographicBuffer.EncodeToBase64String(buffer));

        private static string PublicKeyFingerprint(string pem)
        {
            using var rsa = RSA.Create(); rsa.ImportFromPem(pem);
            if (rsa.KeySize != 2048) throw new CryptographicException("Windows Hello key must be RSA-2048.");
            return Convert.ToHexString(SHA256.HashData(rsa.ExportSubjectPublicKeyInfo())).ToLowerInvariant();
        }

        private async System.Threading.Tasks.Task StageKeyOperationAsync(string draftPath, string outputPath)
        {
            var draft = JsonNode.Parse(File.ReadAllText(draftPath, new UTF8Encoding(false, true))) ?? throw new InvalidDataException("Key-log draft is empty.");
            var draftObj = draft.AsObject();
            if (!draftObj.Select(p => p.Key).ToHashSet(StringComparer.Ordinal).SetEquals(new[] { "schema", "payload", "signature" }) || draftObj["schema"]?.GetValue<int>() != 1 || draftObj["signature"] is not null || draftObj["payload"] is not JsonObject payload)
                throw new InvalidDataException("Key-operation draft must be an unsigned schema-1 envelope with exactly schema, payload, and signature:null.");
            ValidateKeyLogPayload(payload, false);
            using (var key = OpenApprovalKey()) using (var ecdsa = new ECDsaCng(key))
                if (payload["signerKeyId"]?.GetValue<string>() != SpkiFingerprint(ecdsa)) throw new InvalidDataException("Draft signerKeyId does not match this host's local key fingerprint.");
            StageKeyLog(payload, outputPath, draftPath);
        }

        private void StageKeyLog(JsonObject payload, string outputPath, string? draftPath)
        {
            EnsureExclusiveOutput(outputPath);
            var canonical = CanonicalJson(payload);
            var digest = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(canonical))).ToLowerInvariant();
            _record.Text = "Key-log event to sign:\r\n" + JsonSerializer.Serialize(payload, PrettyJson) + "\r\nCanonical payload SHA-256: " + digest;
            _keyLogCommandPayload = (JsonObject)payload.DeepClone();
            _keyLogCommandOutput = outputPath;
            _keyLogCommandDraft = draftPath;
            _sign.Text = "Sign reviewed key-log event";
            _sign.Enabled = true;
            _status.Text = $"Review the complete {payload["operation"]} event & digest, then sign explicitly. Output: {outputPath}";
        }

        private async System.Threading.Tasks.Task SignKeyLogCommandAsync()
        {
            var payload = _keyLogCommandPayload ?? throw new InvalidOperationException("No key-log event is staged.");
            var outputPath = _keyLogCommandOutput ?? throw new InvalidOperationException("No key-log output path is staged.");
            EnsureExclusiveOutput(outputPath);
            if (_keyLogCommandDraft is not null)
            {
                var latest = JsonNode.Parse(File.ReadAllText(_keyLogCommandDraft, new UTF8Encoding(false, true)))?.AsObject() ?? throw new InvalidDataException("Key-log draft is empty.");
                if (!latest.Select(p => p.Key).ToHashSet(StringComparer.Ordinal).SetEquals(new[] { "schema", "payload", "signature" }) ||
                    latest["schema"]?.GetValue<int>() != 1 || latest["signature"] is not null || latest["payload"] is not JsonObject latestPayload)
                    throw new InvalidDataException("Key-log draft changed or is no longer an unsigned schema-1 envelope.");
                ValidateKeyLogPayload(latestPayload, false);
                if (CanonicalJson(latestPayload) != CanonicalJson(payload)) throw new InvalidDataException("Key-log draft changed after review; load command again.");
            }
            var canonical = CanonicalJson(payload);
            var open = await KeyCredentialManager.OpenAsync(KeyLogCredentialName);
            var credential = RequireCredential(open, "Open Windows Hello key");
            var publicPem = PublicKeyPem(credential);
            var localFingerprint = PublicKeyFingerprint(publicPem);
            if (!StringComparer.Ordinal.Equals(payload["signerKeyId"]!.GetValue<string>(), localFingerprint)) throw new InvalidDataException("Draft signerKeyId does not match this host's local Windows Hello key fingerprint.");
            using var writer = new DataWriter();
            writer.WriteBytes(Encoding.UTF8.GetBytes(canonical));
            var signed = await credential.RequestSignAsync(writer.DetachBuffer());
            if (signed.Status != KeyCredentialStatus.Success) throw new OperationCanceledException("Windows Hello signing did not succeed (" + signed.Status + ").");
            var signature = BufferBytes(signed.Result);
            using var rsa = RSA.Create(); rsa.ImportFromPem(publicPem);
            var signedBytes = Encoding.UTF8.GetBytes(canonical);
            if (!rsa.VerifyData(signedBytes, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1) &&
                !rsa.VerifyData(signedBytes, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pss)) throw new CryptographicException("Windows Hello signature failed local RSA/SHA-256 verification.");
            var result = new JsonObject { ["schema"] = 1, ["payload"] = payload.DeepClone(), ["signature"] = new JsonObject
                { ["keyId"] = localFingerprint, ["host"] = Host, ["value"] = Convert.ToBase64String(signature) } };
            AtomicWriteExclusive(outputPath, JsonSerializer.Serialize(result, PrettyJson) + Environment.NewLine);
            Environment.ExitCode = 0;
            Close();
        }

        private static void ValidateKeyLogPayload(JsonObject payload, bool genesis)
        {
            var keys = new[] { "domain", "rootFingerprint", "sequence", "previousHash", "operation", "subject", "signerKeyId" };
            if (!payload.Select(p => p.Key).ToHashSet(StringComparer.Ordinal).SetEquals(keys)) throw new InvalidDataException("Key-log payload has unexpected or missing fields.");
            if (payload["domain"]?.GetValue<string>() != "rightkit.ownership-key-log.v1") throw new InvalidDataException("Unsupported key-log domain.");
            RequireHex(payload, "rootFingerprint"); RequireHex(payload, "signerKeyId");
            var seq = payload["sequence"]?.GetValue<int>() ?? throw new InvalidDataException("Sequence must be an integer.");
            if (seq < 0 || (genesis ? seq != 0 : seq == 0)) throw new InvalidDataException("Sequence is invalid for this event.");
            if (genesis ? payload["previousHash"] is not null : payload["previousHash"] is not JsonValue prev || !prev.TryGetValue<string>(out var hash) || !Regex.IsMatch(hash, "^[a-f0-9]{64}$")) throw new InvalidDataException("previousHash must be null for root or a lowercase SHA-256 value for later events.");
            var operation = payload["operation"]?.GetValue<string>();
            if (operation != (genesis ? "root" : operation) || (!genesis && operation is not ("add" or "revoke"))) throw new InvalidDataException("Operation must be root, add, or revoke as appropriate.");
            if (payload["subject"] is not JsonObject subject) throw new InvalidDataException("Subject must be an object.");
            if (operation == "root" || operation == "add")
            {
                if (!subject.Select(p => p.Key).ToHashSet(StringComparer.Ordinal).SetEquals(new[] { "keyId", "publicKeyPem", "platform", "deviceName" })) throw new InvalidDataException("Key subject has unexpected or missing fields.");
                RequireHex(subject, "keyId"); RequireText(subject, "publicKeyPem", 4096); RequireText(subject, "deviceName", 128);
                if (subject["platform"]?.GetValue<string>() is not ("windows" or "macos")) throw new InvalidDataException("Key platform must be windows or macos.");
                if (PublicKeyFingerprintAny(subject["publicKeyPem"]!.GetValue<string>()) != subject["keyId"]!.GetValue<string>()) throw new InvalidDataException("Subject public key fingerprint does not match keyId.");
            }
            else
            {
                if (!subject.Select(p => p.Key).ToHashSet(StringComparer.Ordinal).SetEquals(new[] { "keyId" })) throw new InvalidDataException("Revoke subject must contain exactly keyId.");
                RequireHex(subject, "keyId");
            }
            if (operation == "root" && (payload["rootFingerprint"]?.GetValue<string>() != subject["keyId"]?.GetValue<string>() || subject["platform"]?.GetValue<string>() != Host)) throw new InvalidDataException("Root fingerprint and platform must match this Windows genesis key.");
        }

        private static void EnsureExclusiveOutput(string path)
        {
            if (File.Exists(path) || Directory.Exists(path)) throw new IOException("Output already exists; operation refuses to replace it.");
        }

        private static void EnsureOutputDirectory(string path)
        {
            var directory = Path.GetDirectoryName(path);
            if (directory is null || !Directory.Exists(directory)) throw new DirectoryNotFoundException("Output directory must already exist.");
        }

        private static void AtomicWriteExclusive(string path, string contents)
        {
            EnsureExclusiveOutput(path);
            var temp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            try { File.WriteAllText(temp, contents, new UTF8Encoding(false)); File.Move(temp, path); }
            finally { if (File.Exists(temp)) File.Delete(temp); }
        }

        private async System.Threading.Tasks.Task ExportPublicKeyAsync()
        {
            var opened = await KeyCredentialManager.OpenAsync(KeyLogCredentialName);
            var keyId = string.Empty;
            string pem;
            if (opened.Status == KeyCredentialStatus.Success && opened.Credential is not null)
            {
                pem = PublicKeyPem(opened.Credential);
                keyId = "win-" + PublicKeyFingerprint(pem);
            }
            else if (opened.Status == KeyCredentialStatus.NotFound)
            {
                using var key = OpenApprovalKey();
                using var ecdsa = new ECDsaCng(key);
                keyId = KeyId(ecdsa);
                pem = ecdsa.ExportSubjectPublicKeyInfoPem();
            }
            else throw new InvalidOperationException("Windows Hello key lookup failed: " + opened.Status + ".");
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
                byte[] signature;
                string signatureKeyId;
                var hello = await KeyCredentialManager.OpenAsync(KeyLogCredentialName);
                if (hello.Status == KeyCredentialStatus.Success && hello.Credential is not null)
                {
                    var credential = hello.Credential;
                    var pem = PublicKeyPem(credential);
                    using var writer = new DataWriter();
                    writer.WriteBytes(Encoding.UTF8.GetBytes(currentCanonical));
                    var result = await credential.RequestSignAsync(writer.DetachBuffer());
                    if (result.Status != KeyCredentialStatus.Success) throw new OperationCanceledException("Windows Hello signing did not succeed (" + result.Status + ").");
                    signature = BufferBytes(result.Result);
                    using var rsa = RSA.Create(); rsa.ImportFromPem(pem);
                    var data = Encoding.UTF8.GetBytes(currentCanonical);
                    if (!rsa.VerifyData(data, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1) &&
                        !rsa.VerifyData(data, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pss)) throw new CryptographicException("Windows Hello signature failed local RSA/SHA-256 verification.");
                    signatureKeyId = "win-" + PublicKeyFingerprint(pem);
                }
                else if (hello.Status == KeyCredentialStatus.NotFound)
                {
                    if (!await VerifyCurrentUserAsync(verificationMessage)) return;
                    using var key = OpenApprovalKey();
                    RequireApprovedKey(key);
                    using var ecdsa = new ECDsaCng(key);
                    signature = ecdsa.SignHash(Convert.FromHexString(digestText), DSASignatureFormat.Rfc3279DerSequence);
                    signatureKeyId = KeyId(ecdsa);
                }
                else throw new InvalidOperationException("Windows Hello key lookup failed: " + hello.Status + ".");
                var signed = current.DeepClone().AsObject();
                signed["signature"] = new JsonObject
                {
                    ["keyId"] = signatureKeyId,
                    ["host"] = Host,
                    ["value"] = Convert.ToBase64String(signature)
                };
                File.WriteAllText(save.FileName, JsonSerializer.Serialize(signed, PrettyJson) + Environment.NewLine, new UTF8Encoding(false));
                _status.Text = "Signed record saved after native key authorization: " + save.FileName;
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
        if (CngKey.Exists(KeyName, Provider)) throw new InvalidOperationException("A host key with this name already exists.");
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

    private static string SpkiFingerprint(ECDsa ecdsa) => Convert.ToHexString(SHA256.HashData(ecdsa.ExportSubjectPublicKeyInfo())).ToLowerInvariant();

    private static string PublicKeyFingerprintAny(string pem)
    {
        try
        {
            using var rsa = RSA.Create(); rsa.ImportFromPem(pem);
            if (rsa.KeySize == 2048) return Convert.ToHexString(SHA256.HashData(rsa.ExportSubjectPublicKeyInfo())).ToLowerInvariant();
        }
        catch (CryptographicException) { }
        using var ecdsa = ECDsa.Create(); ecdsa.ImportFromPem(pem);
        if (ecdsa.KeySize != 256) throw new CryptographicException("Subject key must be RSA-2048 or ECDSA P-256.");
        return SpkiFingerprint(ecdsa);
    }

    // Match JSON.stringify for the scalar types in approval records: preserve JS-compatible
    // UTF-8 and escape only quotes, backslashes, and control characters.
    private static string CanonicalJson(JsonNode? node)
    {
        if (node is null) return "null";
        if (node is JsonObject obj)
            return "{" + string.Join(",", obj.Select(p => p.Key).OrderBy(k => k, StringComparer.Ordinal)
                .Select(k => Quote(k) + ":" + CanonicalJson(obj[k]))) + "}";
        if (node is JsonArray array) return "[" + string.Join(",", array.Select(CanonicalJson)) + "]";
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
