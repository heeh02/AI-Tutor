using System;

namespace SoulLink.Desktop.Voice {
    // In-memory format gate; never records, resamples, truncates, or writes a WAV file.
    internal static class VoiceWave {
        internal static void Validate(byte[] wav) {
            if (wav.Length < 12 || wav.Length > 1024 * 1024 || !Tag(wav, 0, "RIFF") || !Tag(wav, 8, "WAVE")
                || U32(wav, 4) != (uint)(wav.Length - 8)) Invalid();
            bool format = false, pcm = false;
            int offset = 12;
            while (offset < wav.Length) {
                if (wav.Length - offset < 8) Invalid();
                uint length = U32(wav, offset + 4);
                long end = (long)offset + 8 + length;
                long paddedEnd = end + length % 2;
                if (paddedEnd > wav.Length) Invalid();
                int start = offset + 8;
                if (Tag(wav, offset, "fmt ")) {
                    if (format || !(length == 16 || (length == 18 && U16(wav, start + 16) == 0))) Invalid();
                    if (U16(wav, start) != 1 || U16(wav, start + 2) != 1 || U32(wav, start + 4) != 16000
                        || U32(wav, start + 8) != 32000 || U16(wav, start + 12) != 2 || U16(wav, start + 14) != 16) Invalid();
                    format = true;
                } else if (Tag(wav, offset, "data")) {
                    if (pcm || length == 0 || length > 640000 || length % 2 != 0) Invalid();
                    pcm = true;
                }
                offset = (int)paddedEnd;
            }
            if (!format || !pcm) Invalid();
        }
        static bool Tag(byte[] bytes, int at, string tag) {
            for (int i = 0; i < 4; i++) if (bytes[at + i] != tag[i]) return false;
            return true;
        }
        static ushort U16(byte[] bytes, int at) { return (ushort)(bytes[at] | (bytes[at + 1] << 8)); }
        static uint U32(byte[] bytes, int at) { return (uint)(bytes[at] | (bytes[at + 1] << 8) | (bytes[at + 2] << 16) | (bytes[at + 3] << 24)); }
        static void Invalid() { throw new ArgumentException("voice_wav_requires_pcm16_mono_16000_max20s", "wav"); }
    }
}
