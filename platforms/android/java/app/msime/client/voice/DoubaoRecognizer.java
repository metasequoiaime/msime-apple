package app.msime.client;

import android.media.AudioFormat;
import android.media.AudioRecord;
import android.media.MediaRecorder;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.URI;
import java.net.URISyntaxException;
import java.nio.charset.StandardCharsets;
import java.security.SecureRandom;
import java.util.Base64;
import java.util.concurrent.atomic.AtomicBoolean;
import javax.net.ssl.HttpsURLConnection;
import javax.net.ssl.SSLParameters;
import javax.net.ssl.SSLPeerUnverifiedException;
import javax.net.ssl.SSLSocket;
import javax.net.ssl.SSLSocketFactory;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * Streaming recognition over the provider's WebSocket, transcribing while the user speaks.
 *
 * <p>Different in kind from the upload path: the audio leaves as it is captured and the transcript
 * comes back in pieces, so the user sees words appear rather than waiting for a round trip after
 * they stop. That is what the protocol is for, and it is why it is worth carrying a WebSocket
 * implementation for one endpoint.
 *
 * <p>Nothing about the protocol is decided here. The authentication headers arrive already built
 * by the shared policy, and every frame the session sends comes from the shared builders through
 * {@link NativeClient}; this class owns the socket, the microphone and the loop between them.
 */
public final class DoubaoRecognizer {
    private static final int CONNECT_TIMEOUT_MILLIS = 10_000;
    private static final int READ_TIMEOUT_MILLIS = 30_000;
    private static final int MAX_MILLIS = 60_000;
    /** The shared Doubao decoder accepts one-megabyte wire frames; keep room for a WebSocket header. */
    private static final int MAX_INBOUND_FRAME_BYTES = 1_048_576 + 10;
    /** Roughly 100 ms of 16 kHz mono PCM: small enough to stream, large enough not to thrash. */
    private static final int CHUNK_BYTES = 3200;

    /** What the caller does with each update; interim results arrive before the final one. */
    public interface Listener {
        void onUpdate(String text, boolean finalResult);
    }

    private final AtomicBoolean stopped = new AtomicBoolean();
    private final AtomicBoolean cancelled = new AtomicBoolean();
    private final SecureRandom random = new SecureRandom();
    /** The plain socket under TLS, so cancel() can unblock a pending read without TLS I/O. */
    private volatile java.net.Socket transport;

    public void stop() {
        stopped.set(true);
    }

    public void cancel() {
        cancelled.set(true);
        stopped.set(true);
        java.net.Socket active = transport;
        if (active != null) {
            try {
                active.close();
            } catch (IOException | RuntimeException ignored) {
                // The blocked read fails either way, which is the point.
            }
        }
    }

    /**
     * Run one streaming session and return the final transcript, or null.
     *
     * <p>Blocking, and never called on the main thread: it holds the microphone and a socket for
     * as long as the user is speaking.
     */
    public String recognize(String endpoint, String[] headers, boolean itn, boolean punctuation,
                            boolean ddc, String boostingTableId, Listener listener) {
        URI uri = parse(endpoint);
        if (uri == null) return null;
        byte[] start = NativeClient.doubaoStartFrame(itn, punctuation, ddc, boostingTableId);
        if (start == null) return null;
        AudioRecord recorder = null;
        SSLSocket socket = null;
        try {
            socket = connect(uri, headers);
            if (socket == null || cancelled.get()) return null;
            OutputStream out = socket.getOutputStream();
            InputStream in = socket.getInputStream();
            send(out, start);
            recorder = record();
            if (recorder == null) return null;
            return stream(recorder, in, out, listener);
        } catch (IOException error) {
            return null;
        } finally {
            if (recorder != null) {
                stopRecording(recorder);
                recorder.release();
            }
            transport = null;
            if (socket != null) {
                try {
                    socket.close();
                } catch (IOException ignored) {
                    // The session is over either way.
                }
            }
        }
    }

    private String stream(AudioRecord recorder, InputStream in, OutputStream out, Listener listener)
            throws IOException {
        byte[] chunk = new byte[CHUNK_BYTES];
        byte[] inbound = new byte[MAX_INBOUND_FRAME_BYTES];
        int pending = 0;
        int sequence = 1;
        int sent = 0;
        int limit = WavAudio.SAMPLE_RATE * 2 / 1000 * MAX_MILLIS;
        String transcript = null;
        boolean finished = false;
        while (!finished) {
            if (cancelled.get()) return null;
            boolean last = stopped.get() || sent >= limit;
            // Release the microphone before waiting on the final answer.
            if (last) stopRecording(recorder);
            int read = last ? 0 : recorder.read(chunk, 0, chunk.length);
            if (read < 0) return null;
            sent += read;
            byte[] frame = NativeClient.doubaoAudioFrame(sequence++, chunk, read, last);
            if (frame == null) return null;
            send(out, frame);
            // Drain whatever has arrived without blocking the next chunk; after the final frame
            // there is nothing left to send, so waiting for the answer is all that remains.
            while (in.available() > 0 || last) {
                // A zero-length read is not a portable way to ask for more data. If a frame fills
                // the bounded buffer without decoding, reject it instead of handing read() a zero
                // count and silently ending the session with a partial transcript.
                if (pending == inbound.length) return null;
                int got = in.read(inbound, pending, inbound.length - pending);
                if (got <= 0) return transcript;
                pending += got;
                WebSocketFrames.Frame decoded;
                while ((decoded = WebSocketFrames.decode(inbound, pending)) != null) {
                    System.arraycopy(inbound, decoded.consumed(), inbound, 0,
                        pending - decoded.consumed());
                    pending -= decoded.consumed();
                    if (decoded.opcode() == WebSocketFrames.OPCODE_CLOSE) return transcript;
                    if (decoded.opcode() == WebSocketFrames.OPCODE_PING) {
                        sendFrame(out, WebSocketFrames.OPCODE_PONG, decoded.payload());
                        continue;
                    }
                    if (decoded.opcode() != WebSocketFrames.OPCODE_BINARY
                            && decoded.opcode() != WebSocketFrames.OPCODE_CONTINUATION) {
                        continue;
                    }
                    Update update = update(decoded.payload());
                    if (update == null) return transcript;
                    if (update.text != null && !update.text.isEmpty()) {
                        transcript = update.text;
                        if (listener != null) listener.onUpdate(update.text, update.last);
                    }
                    if (update.last) {
                        finished = true;
                        break;
                    }
                }
                if (finished || pending >= inbound.length) break;
            }
            if (last && !finished) return transcript;
        }
        return transcript;
    }

    private record Update(String text, boolean last) {}

    /** One decoded response frame, read through the shared decoder. */
    private Update update(byte[] payload) {
        try {
            JSONObject response = new JSONObject(NativeClient.doubaoDecodeFrame(payload));
            if (!response.optBoolean("ok", false)) return null;
            JSONObject value = response.optJSONObject("value");
            if (value == null) return null;
            // An error frame ends the session; the code is the provider's and is not shown.
            if (value.has("error_code")) return null;
            JSONObject document = new JSONObject(value.optString("payload", "{}"));
            JSONObject result = document.optJSONObject("result");
            String text = result == null ? "" : result.optString("text", "");
            return new Update(text, value.optBoolean("last", false));
        } catch (JSONException error) {
            return null;
        }
    }

    private SSLSocket connect(URI uri, String[] headers) throws IOException {
        int port = uri.getPort() > 0 ? uri.getPort() : 443;
        // Connect first, then hand the connected socket to TLS, so the connect attempt is bounded:
        // SSLSocketFactory.createSocket(host, port) connects with no timeout at all.
        java.net.Socket plain = new java.net.Socket();
        transport = plain;
        SSLSocket socket = null;
        try {
            plain.connect(new java.net.InetSocketAddress(uri.getHost(), port),
                CONNECT_TIMEOUT_MILLIS);
            SSLSocketFactory factory = (SSLSocketFactory) SSLSocketFactory.getDefault();
            socket = (SSLSocket) factory.createSocket(plain, uri.getHost(), port, true);
            // A raw SSLSocket checks the chain but not the hostname unless asked to.
            SSLParameters parameters = socket.getSSLParameters();
            parameters.setEndpointIdentificationAlgorithm("HTTPS");
            socket.setSSLParameters(parameters);
            socket.setSoTimeout(READ_TIMEOUT_MILLIS);
            socket.startHandshake();
            if (!HttpsURLConnection.getDefaultHostnameVerifier()
                    .verify(uri.getHost(), socket.getSession())) {
                throw new SSLPeerUnverifiedException("certificate does not match " + uri.getHost());
            }
            String key = Base64.getEncoder().encodeToString(randomBytes(16));
            String path = uri.getRawPath() == null || uri.getRawPath().isEmpty() ? "/" : uri.getRawPath();
            if (uri.getRawQuery() != null) path = path + "?" + uri.getRawQuery();
            socket.getOutputStream().write(WebSocketFrames
                .handshakeRequest(uri.getHost(), path, key, headers)
                .getBytes(StandardCharsets.US_ASCII));
            socket.getOutputStream().flush();
            String response = readHandshake(socket.getInputStream());
            if (!WebSocketFrames.handshakeAccepted(response, key)) {
                socket.close();
                return null;
            }
            return socket;
        } catch (IOException | RuntimeException error) {
            try {
                if (socket != null) {
                    socket.close();
                } else {
                    plain.close();
                }
            } catch (IOException ignored) {
                // Already failing; the original error is the one worth reporting.
            }
            throw error;
        }
    }

    /** Read exactly the response head, leaving any frame bytes that followed it in the stream. */
    private static String readHandshake(InputStream in) throws IOException {
        StringBuilder head = new StringBuilder();
        int matched = 0;
        while (head.length() < 8192) {
            int value = in.read();
            if (value < 0) break;
            head.append((char) value);
            char expected = "\r\n\r\n".charAt(matched);
            matched = value == expected ? matched + 1 : value == '\r' ? 1 : 0;
            if (matched == 4) break;
        }
        return head.toString();
    }

    private AudioRecord record() {
        int minimum = AudioRecord.getMinBufferSize(WavAudio.SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT);
        if (minimum <= 0) return null;
        AudioRecord recorder;
        try {
            recorder = new AudioRecord(MediaRecorder.AudioSource.VOICE_RECOGNITION,
                WavAudio.SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT, Math.max(minimum, CHUNK_BYTES * 4));
        } catch (IllegalArgumentException | SecurityException error) {
            return null;
        }
        if (recorder.getState() != AudioRecord.STATE_INITIALIZED) {
            recorder.release();
            return null;
        }
        try {
            recorder.startRecording();
        } catch (IllegalStateException error) {
            recorder.release();
            return null;
        }
        if (recorder.getRecordingState() != AudioRecord.RECORDSTATE_RECORDING) {
            recorder.release();
            return null;
        }
        return recorder;
    }

    private static void stopRecording(AudioRecord recorder) {
        try {
            if (recorder.getRecordingState() == AudioRecord.RECORDSTATE_RECORDING) {
                recorder.stop();
            }
        } catch (IllegalStateException ignored) {
            // Already stopped; releasing the microphone is what matters.
        }
    }

    private void send(OutputStream out, byte[] payload) throws IOException {
        sendFrame(out, WebSocketFrames.OPCODE_BINARY, payload);
    }

    private void sendFrame(OutputStream out, int opcode, byte[] payload) throws IOException {
        out.write(WebSocketFrames.clientFrame(opcode, payload, payload.length, mask()));
        out.flush();
    }

    private byte[] mask() {
        return randomBytes(4);
    }

    private byte[] randomBytes(int count) {
        byte[] bytes = new byte[count];
        random.nextBytes(bytes);
        return bytes;
    }

    /** Only `wss://` is accepted: this protocol carries the user's credentials in its headers. */
    static URI parse(String endpoint) {
        if (endpoint == null || !endpoint.startsWith("wss://") || endpoint.length() > 2048) {
            return null;
        }
        try {
            URI uri = new URI(endpoint);
            return uri.getHost() == null || uri.getHost().isEmpty() ? null : uri;
        } catch (URISyntaxException error) {
            return null;
        }
    }
}
