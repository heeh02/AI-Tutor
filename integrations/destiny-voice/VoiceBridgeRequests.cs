using System;
using System.Text;
using System.Threading.Tasks;

namespace SoulLink.Desktop.Voice {
    public sealed partial class VoiceBridgeClient {
        public Task<VoiceResult> StatusAsync() {
            lock (gate) { RequireStarted(); return NewRequest("status", VoiceJson.Object(), null, false).Source.Task; }
        }
        public async Task<VoiceScope> BindAsync(string owner) {
            if (String.IsNullOrWhiteSpace(owner) || Encoding.UTF8.GetByteCount(owner) > 200)
                throw new ArgumentException("voice_scope_invalid", "owner");
            VoiceScope candidate; Task<VoiceResult> response;
            lock (gate) {
                RequireStarted();
                if (active != null && active.Scope != null) active.Revoke();
                candidate = new VoiceScope(owner, NextGeneration()); scope = candidate; bindingReady = false;
                response = NewRequest("bind", VoiceJson.Object("scope", candidate.Wire()), null, false).Source.Task;
            }
            var result = await response.ConfigureAwait(false);
            lock (gate) {
                if (!ReferenceEquals(candidate, scope) || closing || ended) throw VoiceJson.Error("voice_scope_stale");
                if (result.Event != "bound") throw VoiceJson.Error(result.Error ?? "voice_bind_failed");
                bindingReady = true; return candidate;
            }
        }
        public VoiceOperation Prepare() {
            lock (gate) {
                RequireStarted(); RequireIdle();
                return NewRequest("prepare", VoiceJson.Object(), null, true).Operation;
            }
        }
        public VoiceOperation Speak(string text, string language) {
            if (String.IsNullOrWhiteSpace(text) || text.Length > 2000) throw new ArgumentException("voice_speech_text_limit", "text");
            if (language != "zh" && language != "zh-CN" && language != "en" && language != "en-US")
                throw new ArgumentException("voice_language_unsupported", "language");
            lock (gate) {
                RequireMedia();
                return NewRequest("speak", VoiceJson.Object("scope", scope.Wire(), "text", text, "language", language), scope, true).Operation;
            }
        }
        public VoiceOperation Transcribe(byte[] wav) {
            // Clone before validation/encoding so caller mutation cannot change the validated input.
            if (wav == null || wav.Length > 1024 * 1024) throw new ArgumentException("voice_wav_size_invalid", "wav");
            var owned = (byte[])wav.Clone();
            try {
                VoiceWave.Validate(owned);
                lock (gate) {
                    RequireMedia();
                    return NewRequest("transcribe", VoiceJson.Object("scope", scope.Wire(), "wav_base64", Convert.ToBase64String(owned)), scope, true).Operation;
                }
            } finally { Array.Clear(owned, 0, owned.Length); }
        }
        public Task<VoiceResult> CancelAsync(VoiceOperation operation) {
            lock (gate) {
                if (operation == null || operation.Owner != this) throw new ArgumentException("voice_operation_not_owned", "operation");
                operation.Revoke(); // Delivery is fenced before any pipe write or stop receipt.
                if (active != operation || closing || ended)
                    return Task.FromResult(new VoiceResult("rejected", null, 0, VoiceJson.Object("error", "voice_operation_not_active"), true));
                return NewRequest("cancel", VoiceJson.Object("operation_id", operation.Id,
                    "operation_sequence", operation.Sequence), null, false).Source.Task;
            }
        }
        // For release/logout: revoke delivery immediately; caller still awaits cancellation/retirement.
        public Task<VoiceResult> InvalidateBindingAsync() {
            lock (gate) {
                scope = null; bindingReady = false;
                if (active != null && active.Scope != null) return CancelAsync(active);
                return Task.FromResult(new VoiceResult("invalidated", null, 0, VoiceJson.Object(), true));
            }
        }
        void RequireIdle() { if (active != null) throw VoiceJson.Error("voice_busy"); }
        void RequireMedia() {
            RequireStarted(); RequireIdle();
            if (scope == null || !bindingReady) throw VoiceJson.Error("voice_scope_unbound");
        }
    }
}
