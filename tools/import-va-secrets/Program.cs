// One-time import only. The Rust application never depends on .NET or this vault.
using System.Diagnostics;
using System.Text.Json;
using NeoSmart.SecureStore;

if (args.Length != 3)
{
    Console.Error.WriteLine("Usage: ImportSecrets <vault> <key-file> <selection-json>");
    return 2;
}
try
{
    using var secrets = SecretsManager.LoadStore(args[0]);
    secrets.LoadKeyFromFile(args[1]);
    var specs = JsonSerializer.Deserialize<Selection[]>(File.ReadAllText(args[2]),
        new JsonSerializerOptions { PropertyNameCaseInsensitive = true }) ?? [];
    if (specs.Length > 32) throw new InvalidOperationException();
    var stored = 0;
    foreach (var spec in specs)
    {
        var vaultKey = secrets.Keys.FirstOrDefault(k => string.Equals(k, spec.VaultKey, StringComparison.OrdinalIgnoreCase));
        if (vaultKey is null)
        {
            if (spec.Required) throw new InvalidOperationException();
            continue;
        }
        var secret = secrets.Get(vaultKey);
        if (string.IsNullOrWhiteSpace(secret)) continue;
        var info = new ProcessStartInfo("secret-tool")
        {
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
        };
        foreach (var argument in new[] { "store", "--label", "SIE speech credential",
            "application", "sie-tts", "credential", spec.Reference })
            info.ArgumentList.Add(argument);
        using var process = Process.Start(info) ?? throw new InvalidOperationException();
        await process.StandardInput.WriteAsync(secret);
        process.StandardInput.Close();
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        try { await process.WaitForExitAsync(timeout.Token); }
        catch { process.Kill(); throw; }
        if (process.ExitCode != 0) throw new InvalidOperationException();
        stored++;
    }
    Console.WriteLine($"Stored {stored} TTS credentials in the desktop keyring.");
    return 0;
}
catch
{
    // Exception messages from vault/keyring libraries may contain sensitive data.
    Console.Error.WriteLine("Credential import failed; verify vault access and the unlocked desktop keyring.");
    return 1;
}

record Selection(string VaultKey, string Reference, bool Required);
