using System;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace SoulLink.Desktop.Voice {
    public sealed class OwnedVoiceProcess : IVoiceBridgeTransport {
        readonly string executable, root;
        readonly object writeGate = new object();
        readonly TaskCompletionSource<int> exit = new TaskCompletionSource<int>();
        Task writes = Task.FromResult(0);
        Task outputReader, errorReader;
        Process process;
        StreamWriter input;
        long stderrCharacters;
        bool started, inputClosed, disposed;
        int closed;
        public event Action<string> LineReceived;
        public event Action<string> Closed;
        public Task<int> Exit { get { return exit.Task; } }
        public long StderrCharacterCount { get { return Interlocked.Read(ref stderrCharacters); } }
        public OwnedVoiceProcess(string executablePath, string voiceRoot) {
            if (String.IsNullOrWhiteSpace(executablePath) || String.IsNullOrWhiteSpace(voiceRoot))
                throw new ArgumentException("voice_paths_required");
            executable = Path.GetFullPath(executablePath); root = Path.GetFullPath(voiceRoot);
        }
        public void Start() {
            lock (writeGate) {
                if (started || disposed) throw VoiceJson.Error("voice_process_already_started");
                started = true;
                process = new Process { StartInfo = new ProcessStartInfo {
                    FileName = executable, Arguments = "--root " + QuoteArgument(root),
                    UseShellExecute = false, CreateNoWindow = true,
                    RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true,
                    StandardOutputEncoding = new UTF8Encoding(false, true),
                    StandardErrorEncoding = new UTF8Encoding(false, false)
                }};
                try {
                    if (!process.Start()) throw VoiceJson.Error("voice_process_start_failed");
                    input = new StreamWriter(process.StandardInput.BaseStream, new UTF8Encoding(false, true));
                    input.NewLine = "\n";
                    outputReader = Task.Factory.StartNew(ReadOutput, CancellationToken.None, TaskCreationOptions.LongRunning, TaskScheduler.Default);
                    errorReader = Task.Factory.StartNew(DrainError, CancellationToken.None, TaskCreationOptions.LongRunning, TaskScheduler.Default);
                    Task.Factory.StartNew(ObserveExit, CancellationToken.None, TaskCreationOptions.LongRunning, TaskScheduler.Default);
                } catch {
                    exit.TrySetException(VoiceJson.Error("voice_process_start_failed"));
                    throw VoiceJson.Error("voice_process_start_failed");
                }
            }
        }
        public Task SendAsync(string line) {
            lock (writeGate) {
                if (!started || inputClosed || disposed) return Fault("voice_input_closed");
                // Queue creation and sequence allocation are serialized by the client.
                writes = writes.ContinueWith(async previous => {
                    await previous.ConfigureAwait(false);
                    await input.WriteLineAsync(line).ConfigureAwait(false);
                    await input.FlushAsync().ConfigureAwait(false);
                }, CancellationToken.None, TaskContinuationOptions.None, TaskScheduler.Default).Unwrap();
                return writes;
            }
        }
        static Task Fault(string code) {
            var result = new TaskCompletionSource<int>(); result.SetException(VoiceJson.Error(code)); return result.Task;
        }
        void ReadOutput() {
            string reason = "voice_helper_eof";
            try {
                var line = new StringBuilder();
                int next;
                while ((next = process.StandardOutput.Read()) != -1) {
                        char c = (char)next;
                        if (c == '\n') {
                            string complete = line.ToString().TrimEnd('\r'); line.Clear();
                            var callback = LineReceived; if (callback != null) callback(complete);
                        } else {
                            if (line.Length >= 1500000) throw VoiceJson.Error("voice_output_frame_limit");
                            line.Append(c);
                        }
                }
                if (line.Length != 0) reason = "voice_output_truncated";
            } catch { reason = "voice_output_unavailable"; }
            NotifyClosed(reason);
        }
        void DrainError() {
            try {
                var buffer = new char[4096]; int count;
                while ((count = process.StandardError.Read(buffer, 0, buffer.Length)) != 0) {
                    Interlocked.Add(ref stderrCharacters, count);
                    Array.Clear(buffer, 0, count);
                }
            } catch { /* Diagnostic contents must never enter an exception or log. */ }
        }
        void ObserveExit() {
            try {
                process.WaitForExit();
                Task.WaitAll(outputReader, errorReader);
                exit.TrySetResult(process.ExitCode);
            }
            catch { exit.TrySetException(VoiceJson.Error("voice_exit_unavailable")); NotifyClosed("voice_exit_unavailable"); }
        }
        void NotifyClosed(string reason) {
            if (Interlocked.Exchange(ref closed, 1) != 0) return;
            var callback = Closed; if (callback != null) callback(reason);
        }
        public void CloseInput() {
            lock (writeGate) {
                if (inputClosed) return; inputClosed = true;
                if (input != null) input.Close();
            }
        }
        public void Kill() { if (process != null && !process.HasExited) process.Kill(); }
        public void Dispose() {
            lock (writeGate) {
                if (disposed) return;
                if (started && !exit.Task.IsCompleted) throw VoiceJson.Error("voice_process_still_running");
                disposed = true;
                if (input != null) input.Dispose();
                if (process != null) process.Dispose();
            }
        }
        internal static string QuoteArgument(string value) {
            var result = new StringBuilder("\""); int slashes = 0;
            foreach (char c in value) {
                if (c == '\\') { slashes++; continue; }
                if (c == '"') result.Append('\\', slashes * 2 + 1);
                else result.Append('\\', slashes);
                result.Append(c); slashes = 0;
            }
            result.Append('\\', slashes * 2); return result.Append('"').ToString();
        }
    }
}
