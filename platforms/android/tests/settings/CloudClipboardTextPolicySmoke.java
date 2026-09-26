import app.msime.client.clipboard.CloudClipboardTextPolicy;

public final class CloudClipboardTextPolicySmoke {
    public static void main(String[] args) {
        check(CloudClipboardTextPolicy.valid("synthetic text"), "ordinary text is accepted");
        check(CloudClipboardTextPolicy.valid("line\ncolumn\tvalue\r"),
            "clipboard line breaks and tabs are accepted");
        check(CloudClipboardTextPolicy.valid("x".repeat(4_000)),
            "the inclusive UTF-16 limit is accepted");
        check(!CloudClipboardTextPolicy.valid("x".repeat(4_001)),
            "text over the service limit is rejected");
        check(CloudClipboardTextPolicy.valid("😀".repeat(2_000)),
            "the UTF-16 boundary accepts two thousand supplementary characters");
        check(!CloudClipboardTextPolicy.valid("😀".repeat(2_001)),
            "the UTF-16 boundary rejects the next supplementary character");
        check(!CloudClipboardTextPolicy.valid("   \n\t"), "blank text is rejected");
        check(!CloudClipboardTextPolicy.valid("\u2003\u00a0"), "Unicode whitespace is rejected");
        check(!CloudClipboardTextPolicy.valid("safe\u0000hidden"), "NUL is rejected");
        check(!CloudClipboardTextPolicy.valid("safe\u0007hidden"), "other controls are rejected");
        check(!CloudClipboardTextPolicy.valid(null), "null is rejected");
        System.out.println("Android cloud clipboard: shared text boundary passed");
    }

    private static void check(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
