package app.msime.client.home;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.os.Bundle;
import android.view.View;
import android.widget.LinearLayout;
import android.widget.TextView;
import androidx.annotation.Nullable;
import androidx.appcompat.app.AppCompatActivity;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;
import com.google.android.material.appbar.MaterialToolbar;
import com.google.android.material.button.MaterialButton;
import com.google.android.material.switchmaterial.SwitchMaterial;
import com.google.android.material.textfield.TextInputEditText;
import app.msime.client.BackendAccount;
import app.msime.client.clipboard.CloudClipboardTextPolicy;
import app.msime.client.R;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/** Account-owned cloud clipboard. It never reads the Android clipboard automatically. */
public final class CloudClipboardActivity extends AppCompatActivity {
    private final ExecutorService worker = Executors.newSingleThreadExecutor();
    private SwitchMaterial enabled;
    private TextInputEditText search;
    private TextInputEditText draft;
    private LinearLayout items;
    private TextView status;
    private boolean busy;

    @Override protected void onCreate(@Nullable Bundle state) {
        super.onCreate(state);
        WindowCompat.setDecorFitsSystemWindows(getWindow(), false);
        setContentView(R.layout.activity_cloud_clipboard);
        View root = findViewById(R.id.cloud_clipboard_root);
        ViewCompat.setOnApplyWindowInsetsListener(root, (view, insets) -> {
            Insets bars = insets.getInsets(WindowInsetsCompat.Type.systemBars());
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom);
            return insets;
        });
        MaterialToolbar toolbar = findViewById(R.id.cloud_clipboard_bar);
        toolbar.setNavigationOnClickListener(ignored -> finish());
        enabled = findViewById(R.id.cloud_clipboard_enabled);
        search = findViewById(R.id.cloud_clipboard_search);
        draft = findViewById(R.id.cloud_clipboard_draft);
        items = findViewById(R.id.cloud_clipboard_items);
        status = findViewById(R.id.cloud_clipboard_status);
        enabled.setOnCheckedChangeListener((button, checked) -> {
            if (!button.isPressed() || busy) return;
            run(() -> { new BackendAccount(this).setClipboardEnabled(checked); return null; });
        });
        findViewById(R.id.cloud_clipboard_refresh).setOnClickListener(ignored -> reload());
        findViewById(R.id.cloud_clipboard_add).setOnClickListener(ignored -> add());
        reload();
    }

    private void reload() {
        if (busy) return;
        String query = search == null || search.getText() == null ? "" : search.getText().toString();
        run(() -> new BackendAccount(this).clipboard(query), this::render);
    }

    private void add() {
        String text = draft.getText() == null ? "" : draft.getText().toString();
        if (!CloudClipboardTextPolicy.valid(text)) {
            status.setText("请输入有效且不超过 4,000 个 UTF-16 单元的内容");
            return;
        }
        run(() -> { new BackendAccount(this).addClipboard(text); return null; }, ignored -> {
            draft.setText("");
            reload();
        });
    }

    private void render(BackendAccount.ClipboardPage page) {
        enabled.setChecked(page.enabled());
        items.removeAllViews();
        if (page.items().isEmpty()) {
            TextView empty = new TextView(this);
            empty.setText(search.getText() == null || search.getText().length() == 0
                ? "还没有保存任何内容" : "没有匹配的内容");
            empty.setPadding(0, 16, 0, 16);
            items.addView(empty);
        }
        for (BackendAccount.ClipboardItem item : page.items()) {
            MaterialButton row = new MaterialButton(this);
            row.setText(item.text());
            row.setGravity(android.view.Gravity.START | android.view.Gravity.CENTER_VERTICAL);
            row.setMaxLines(3);
            row.setOnClickListener(ignored -> {
                ClipboardManager clipboard = getSystemService(ClipboardManager.class);
                if (clipboard != null) clipboard.setPrimaryClip(ClipData.newPlainText("水杉云剪贴板", item.text()));
                status.setText("已复制到系统剪贴板");
            });
            row.setOnLongClickListener(ignored -> {
                run(() -> { new BackendAccount(this).deleteClipboard(item.id()); return null; }, ignored2 -> reload());
                return true;
            });
            items.addView(row);
        }
        if (!page.items().isEmpty()) {
            MaterialButton clear = new MaterialButton(this);
            clear.setText("清空历史");
            clear.setOnClickListener(ignored -> run(
                () -> { new BackendAccount(this).deleteClipboard(null); return null; },
                ignored2 -> reload()));
            items.addView(clear);
        }
        status.setText("最多保存 50 条；点按复制，长按删除");
    }

    private interface Work<T> { T run() throws Exception; }
    private <T> void run(Work<T> work) { run(work, ignored -> reload()); }
    private <T> void run(Work<T> work, java.util.function.Consumer<T> done) {
        if (busy) return;
        busy = true;
        status.setText("处理中…");
        worker.execute(() -> {
            try {
                T result = work.run();
                runOnUiThread(() -> {
                    if (isFinishing() || isDestroyed()) return;
                    busy = false;
                    done.accept(result);
                });
            } catch (Exception error) {
                runOnUiThread(() -> {
                    if (isFinishing() || isDestroyed()) return;
                    busy = false;
                    status.setText("连接未完成，请登录后重试");
                });
            }
        });
    }

    @Override protected void onDestroy() {
        worker.shutdownNow();
        super.onDestroy();
    }
}
