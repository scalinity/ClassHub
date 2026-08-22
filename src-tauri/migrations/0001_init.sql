CREATE TABLE classes (
    id INTEGER PRIMARY KEY,
    folder_name TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    code TEXT NOT NULL,
    color TEXT NOT NULL,
    room TEXT NOT NULL,
    instructors TEXT NOT NULL,
    credits INTEGER NOT NULL,
    grading_basis TEXT NOT NULL,
    final_exam_start TEXT,
    final_exam_end TEXT
);

CREATE TABLE meetings (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    weekday INTEGER NOT NULL, -- 1=Mon .. 7=Sun
    start_time TEXT NOT NULL,
    end_time TEXT NOT NULL,
    periods TEXT NOT NULL
);

CREATE TABLE files (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    rel_path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    kind TEXT NOT NULL, -- pptx|pdf|rmd|r|html|md|other
    extract_rel_path TEXT,
    extracted_at INTEGER,
    extracted_sha256 TEXT, -- hash of source when extract was made
    UNIQUE(class_id, rel_path)
);

CREATE TABLE jobs (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL, -- extract|module_guide|master_guide|sort_proposal|syllabus_scan|practice
    class_id INTEGER REFERENCES classes(id),
    scope TEXT, -- e.g. module rel path, or 'master'
    status TEXT NOT NULL, -- queued|running|succeeded|failed|cancelled
    session_id TEXT, -- claude session id (for --resume)
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    log_path TEXT,
    error TEXT,
    summary TEXT
);

CREATE TABLE guides (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    scope TEXT NOT NULL, -- module rel path | 'master'
    rel_path TEXT NOT NULL,
    generated_at INTEGER NOT NULL,
    source_manifest TEXT NOT NULL, -- JSON: [{rel_path, sha256}] used for staleness
    UNIQUE(class_id, scope)
);

CREATE TABLE deadlines (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    title TEXT NOT NULL,
    kind TEXT NOT NULL, -- assignment|exam|quiz|project|other
    due_at TEXT NOT NULL,
    notes TEXT,
    status TEXT NOT NULL, -- open|done
    source TEXT NOT NULL -- manual|agent|syllabus
);

CREATE TABLE grade_categories (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    name TEXT NOT NULL,
    weight REAL NOT NULL
);

CREATE TABLE grade_items (
    id INTEGER PRIMARY KEY,
    category_id INTEGER NOT NULL REFERENCES grade_categories(id),
    name TEXT NOT NULL,
    score REAL NOT NULL,
    max_score REAL NOT NULL,
    graded_at TEXT
);

CREATE TABLE chat_sessions (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE chat_messages (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES chat_sessions(id),
    role TEXT NOT NULL,
    content TEXT NOT NULL, -- JSON: full Messages-API content blocks
    created_at INTEGER NOT NULL
);

CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY,
    action TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT INTO classes (id, folder_name, display_name, code, color, room, instructors, credits, grading_basis, final_exam_start, final_exam_end) VALUES
    (1, 'Fundamentals of Artificial Intelligence in Medicine I', 'Fundamentals of AI in Medicine I', 'CAI5720', 'blue', 'JAX1 231', 'Zhenhong Hu, Yijiang Chen', 3, 'Letter Grade', NULL, NULL),
    (2, 'AI in Health Design Studio I', 'AI in Health Design Studio I', 'CAI5724', 'orange', 'JAX1 231', 'Benjamin Shickel', 1, 'Letter Grade', '2026-12-07T20:00:00', '2026-12-07T22:00:00'),
    (3, 'Biostatistics for AI', 'Biostatistics for AI', 'CAI5731', 'green', 'JAX1 231', 'Esra Adiyeke, Tezcan Ozrazgat Baslanti', 2, 'Letter Grade', NULL, NULL),
    (4, 'Applied Generative AI in Medicine', 'Applied Generative AI in Medicine', 'CAI6734', 'amber', 'JAX1 231', 'Akshith Ullal, Xuefeng Liu', 3, 'Letter Grade', NULL, NULL);

INSERT INTO meetings (class_id, weekday, start_time, end_time, periods) VALUES
    (1, 2, '16:05', '19:05', '9-11'),
    (2, 3, '17:10', '18:00', '10'),
    (3, 4, '11:45', '13:40', '5-6'),
    (4, 2, '11:45', '14:45', '5-7');
