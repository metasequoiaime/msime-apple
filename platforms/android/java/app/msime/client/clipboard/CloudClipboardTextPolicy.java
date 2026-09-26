package app.msime.client.clipboard;

/** The account API's cloud clipboard text contract, shared by validation and the native screen. */
public final class CloudClipboardTextPolicy {
    /** The service counts UTF-16 units for this field, as does the shared client-core validator. */
    public static final int MAX_UTF16_UNITS = 4_000;

    private CloudClipboardTextPolicy() {}

    public static boolean valid(String text) {
        if (text == null || blank(text) || text.length() > MAX_UTF16_UNITS) {
            return false;
        }
        for (int index = 0; index < text.length(); index++) {
            char character = text.charAt(index);
            if (character == '\u0000'
                    || (Character.isISOControl(character) && character != '\n'
                    && character != '\r' && character != '\t')) {
                return false;
            }
        }
        return true;
    }

    private static boolean blank(String text) {
        if (text.isEmpty()) return true;
        for (int offset = 0; offset < text.length();) {
            int codePoint = text.codePointAt(offset);
            if (!Character.isWhitespace(codePoint) && !Character.isSpaceChar(codePoint)) return false;
            offset += Character.charCount(codePoint);
        }
        return true;
    }
}
