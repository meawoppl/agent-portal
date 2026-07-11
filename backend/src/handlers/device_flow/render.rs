pub(super) fn render_device_code_form(
    user_code: Option<&str>,
    error_message: Option<&str>,
) -> String {
    let value = escape_html_text(user_code.unwrap_or(""));
    let error = error_message
        .map(|message| {
            format!(
                r#"<p class="error-message">{}</p>"#,
                escape_html_text(message)
            )
        })
        .unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover">
    <title>Device Authentication - Agent Portal</title>
    <style>
        :root {{
            --bg-dark: #1a1b26;
            --bg-darker: #16161e;
            --text-primary: #c0caf5;
            --text-secondary: #7f849c;
            --accent: #7aa2f7;
            --accent-hover: #9eb3ff;
            --border: #292e42;
            --error: #f7768e;
        }}
        * {{
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }}
        body {{
            font-family: 'Segoe UI', Tahoma, Geneva, Verdana, sans-serif;
            background: var(--bg-dark);
            color: var(--text-primary);
            min-height: 100dvh;
            display: flex;
            align-items: center;
            justify-content: center;
            padding: max(1rem, env(safe-area-inset-top)) max(1rem, env(safe-area-inset-right)) max(1rem, env(safe-area-inset-bottom)) max(1rem, env(safe-area-inset-left));
        }}
        .container {{
            background: var(--bg-darker);
            border: 1px solid var(--border);
            border-radius: 12px;
            padding: 2rem;
            max-width: 400px;
            width: min(100%, 400px);
            text-align: center;
        }}
        h1 {{
            font-size: 1.5rem;
            margin-bottom: 0.5rem;
            color: var(--accent);
        }}
        p {{
            color: var(--text-secondary);
            margin-bottom: 1.5rem;
            font-size: 0.9rem;
            line-height: 1.45;
        }}
        .error-message {{
            color: var(--error);
            margin-bottom: 1rem;
        }}
        .code-input {{
            width: 100%;
            padding: 1rem;
            font-size: 1.5rem;
            text-align: center;
            background: var(--bg-dark);
            border: 2px solid var(--border);
            border-radius: 8px;
            color: var(--text-primary);
            font-family: 'Courier New', monospace;
            letter-spacing: 0.25rem;
            text-transform: uppercase;
            margin-bottom: 1rem;
        }}
        .code-input:focus {{
            outline: none;
            border-color: var(--accent);
        }}
        .code-input::placeholder {{
            color: var(--text-secondary);
            letter-spacing: normal;
            text-transform: none;
        }}
        button {{
            width: 100%;
            min-height: 48px;
            padding: 0.75rem 1.5rem;
            font-size: 1rem;
            background: var(--accent);
            color: var(--bg-dark);
            border: none;
            border-radius: 8px;
            cursor: pointer;
            font-weight: 600;
            transition: background 0.2s;
        }}
        button:hover {{
            background: var(--accent-hover);
        }}
        .hint {{
            margin: 1rem 0 0;
            font-size: 0.8rem;
            color: var(--text-secondary);
        }}
        @media (max-width: 480px) {{
            .container {{
                padding: 1.5rem;
            }}
            .code-input {{
                font-size: 1.25rem;
                letter-spacing: 0.18rem;
            }}
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1>Device Authentication</h1>
        <p>Enter the code displayed in your terminal to authenticate this device.</p>
        {error}
        <form action="/api/auth/device" method="get">
            <input
                type="text"
                name="user_code"
                class="code-input"
                placeholder="XXX-XXX"
                pattern="[A-Za-z0-9]{{3}}-?[A-Za-z0-9]{{3}}"
                maxlength="7"
                value="{value}"
                inputmode="text"
                autocomplete="one-time-code"
                autocapitalize="characters"
                required
                autofocus
            >
            <button type="submit">Continue</button>
        </form>
        <p class="hint">The code is shown in your terminal after running <code>claude-portal</code></p>
    </div>
    <script>
        const input = document.querySelector('.code-input');
        input.addEventListener('input', () => {{
            const compact = input.value.replace(/[^a-z0-9]/gi, '').slice(0, 6).toUpperCase();
            input.value = compact.length > 3 ? compact.slice(0, 3) + '-' + compact.slice(3) : compact;
        }});
    </script>
</body>
</html>"#
    )
}

pub(super) fn render_approval_page(
    user_code: &str,
    hostname: Option<&str>,
    working_directory: Option<&str>,
) -> String {
    let hostname_display = escape_html_text(hostname.unwrap_or("Unknown device"));
    let working_dir_display = escape_html_text(
        working_directory
            .map(|wd| {
                // Extract just the last component (likely repo name)
                std::path::Path::new(wd)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(wd)
            })
            .unwrap_or("Unknown directory"),
    );
    let user_code_display = escape_html_text(user_code);
    let user_code_json = serde_json::to_string(user_code).unwrap_or_else(|_| "\"\"".to_string());

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover">
    <title>Approve Device - Agent Portal</title>
    <style>
        :root {{
            --bg-dark: #1a1b26;
            --bg-darker: #16161e;
            --text-primary: #c0caf5;
            --text-secondary: #7f849c;
            --accent: #7aa2f7;
            --accent-hover: #9eb3ff;
            --border: #292e42;
            --success: #9ece6a;
            --error: #f7768e;
            --warning: #e0af68;
        }}
        * {{ margin: 0; padding: 0; box-sizing: border-box; }}
        body {{
            font-family: 'Segoe UI', Tahoma, Geneva, Verdana, sans-serif;
            background: var(--bg-dark);
            color: var(--text-primary);
            min-height: 100dvh;
            display: flex;
            align-items: center;
            justify-content: center;
            padding: max(1rem, env(safe-area-inset-top)) max(1rem, env(safe-area-inset-right)) max(1rem, env(safe-area-inset-bottom)) max(1rem, env(safe-area-inset-left));
        }}
        .container {{
            background: var(--bg-darker);
            border: 1px solid var(--border);
            border-radius: 12px;
            padding: 2rem;
            max-width: 450px;
            width: min(100%, 450px);
            text-align: center;
        }}
        h1 {{
            font-size: 1.5rem;
            margin-bottom: 0.5rem;
            color: var(--warning);
        }}
        .subtitle {{
            color: var(--text-secondary);
            margin-bottom: 1.5rem;
            font-size: 0.9rem;
        }}
        .device-info {{
            background: var(--bg-dark);
            border: 1px solid var(--border);
            border-radius: 8px;
            padding: 1rem;
            margin-bottom: 1.5rem;
            text-align: left;
        }}
        .device-info .label {{
            color: var(--text-secondary);
            font-size: 0.75rem;
            text-transform: uppercase;
            letter-spacing: 0.05em;
            margin-bottom: 0.25rem;
        }}
        .device-info .value {{
            color: var(--text-primary);
            font-family: 'Courier New', monospace;
            font-size: 0.95rem;
            margin-bottom: 0.75rem;
            word-break: break-all;
        }}
        .device-info .value:last-child {{
            margin-bottom: 0;
        }}
        .code-display {{
            background: var(--bg-dark);
            border: 2px solid var(--accent);
            border-radius: 8px;
            padding: 0.75rem;
            font-family: 'Courier New', monospace;
            font-size: 1.25rem;
            letter-spacing: 0.2rem;
            color: var(--accent);
            margin-bottom: 1.5rem;
        }}
        .buttons {{
            display: flex;
            gap: 1rem;
        }}
        button {{
            flex: 1;
            min-height: 48px;
            padding: 0.75rem 1.5rem;
            font-size: 1rem;
            border: none;
            border-radius: 8px;
            cursor: pointer;
            font-weight: 600;
            transition: all 0.2s;
        }}
        .approve {{
            background: var(--success);
            color: var(--bg-dark);
        }}
        .approve:hover {{
            filter: brightness(1.1);
        }}
        .deny {{
            background: transparent;
            border: 1px solid var(--error);
            color: var(--error);
        }}
        .deny:hover {{
            background: var(--error);
            color: var(--bg-dark);
        }}
        .warning {{
            color: var(--text-secondary);
            font-size: 0.8rem;
            margin-top: 1rem;
        }}
        .result {{
            display: none;
            padding: 1rem;
            border-radius: 8px;
            margin-top: 1rem;
        }}
        .result.success {{
            background: rgba(158, 206, 106, 0.1);
            border: 1px solid var(--success);
            color: var(--success);
        }}
        .result.error {{
            background: rgba(247, 118, 142, 0.1);
            border: 1px solid var(--error);
            color: var(--error);
        }}
        @media (max-width: 480px) {{
            .container {{
                padding: 1.5rem;
            }}
            .buttons {{
                flex-direction: column-reverse;
                gap: 0.75rem;
            }}
            .code-display {{
                font-size: 1.1rem;
                letter-spacing: 0.14rem;
            }}
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1>⚠️ Authorize Device?</h1>
        <p class="subtitle">A device is requesting access to your Claude Code sessions</p>

        <div class="device-info">
            <div class="label">Machine</div>
            <div class="value">{hostname_display}</div>
            <div class="label">Directory</div>
            <div class="value">{working_dir_display}</div>
        </div>

        <div class="code-display">{user_code_display}</div>

        <div class="buttons">
            <button class="deny" onclick="denyDevice()">Deny</button>
            <button class="approve" onclick="approveDevice()">Approve</button>
        </div>

        <div id="result" class="result"></div>

        <p class="warning">Only approve if you initiated this request from your terminal.</p>
    </div>

    <script>
        const userCode = {user_code_json};

        async function approveDevice() {{
            try {{
                const response = await fetch('/api/auth/device/approve', {{
                    method: 'POST',
                    headers: {{ 'Content-Type': 'application/json' }},
                    body: JSON.stringify({{ user_code: userCode }})
                }});
                const data = await response.json();
                if (response.ok) {{
                    showResult('success', 'Device authorized! You can close this page or return to the dashboard.');
                    setTimeout(() => window.location.href = '/dashboard', 2000);
                }} else {{
                    showResult('error', data.message || 'Failed to authorize device');
                }}
            }} catch (e) {{
                showResult('error', 'Network error: ' + e.message);
            }}
        }}

        async function denyDevice() {{
            try {{
                const response = await fetch('/api/auth/device/deny', {{
                    method: 'POST',
                    headers: {{ 'Content-Type': 'application/json' }},
                    body: JSON.stringify({{ user_code: userCode }})
                }});
                const data = await response.json();
                if (response.ok) {{
                    showResult('error', 'Device authorization denied.');
                    setTimeout(() => window.location.href = '/', 1500);
                }} else {{
                    showResult('error', data.message || 'Failed to deny device');
                }}
            }} catch (e) {{
                showResult('error', 'Network error: ' + e.message);
            }}
        }}

        function showResult(type, message) {{
            const result = document.getElementById('result');
            result.className = 'result ' + type;
            result.textContent = message;
            result.style.display = 'block';
            document.querySelector('.buttons').style.display = 'none';
        }}
    </script>
</body>
</html>"#
    )
}

pub(super) fn escape_html_text(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
