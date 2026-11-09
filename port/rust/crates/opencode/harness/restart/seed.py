# Seeds a database with backgrounded job rows for harness/restart/run.sh: shell jobs that were running, completed and
# failed, an image job, a subagent whose child session is gone, and a job of a session that does not exist.
import json, sqlite3, sys

db = sqlite3.connect(sys.argv[1])
session = "ses_00000000000000restartcheck"
rows = [
    ("msg_00000000000000restartnote1", "job_shell_running", "shell", "running", None, None,
     {"kind": "shell", "sessionID": session, "shellID": "shl_running", "command": "sleep 100"}),
    ("msg_00000000000000restartnote2", "job_shell_done", "shell", "completed", "all done\n", None,
     {"kind": "shell", "sessionID": session, "shellID": "shl_done", "command": "echo all done"}),
    ("msg_00000000000000restartnote3", "job_shell_failed", "shell", "error", None, "boom",
     {"kind": "shell", "sessionID": session, "shellID": "shl_failed", "command": "false"}),
    ("msg_00000000000000restartnote4", "job_image", "image", "running", None, None,
     {"kind": "image", "sessionID": session, "path": "out.png", "model": "openrouter/x"}),
    ("msg_00000000000000restartnote5", "ses_00000000000000restartchild", "subagent", "running", None, None,
     {"kind": "subagent", "parentSessionID": session, "childSessionID": "ses_00000000000000restartchild", "agent": "general", "description": "Missing child"}),
    ("msg_00000000000000restartnote6", "job_other", "shell", "running", None, None,
     {"kind": "shell", "sessionID": "ses_00000000000000restartgone", "shellID": "shl_gone", "command": "ls"}),
]
for index, (notification, id, kind, status, output, error, recovery) in enumerate(rows):
    db.execute(
        "INSERT INTO job (notification_id, id, session_id, child_session_id, kind, title, status, output, error, recovery, time_created, time_updated) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
        (notification, id, recovery.get("sessionID") or recovery.get("parentSessionID"), recovery.get("childSessionID"), kind,
         recovery.get("command") or recovery.get("description") or recovery.get("path"), status, output, error, json.dumps(recovery), 1000 + index, 1000 + index),
    )
db.commit()
