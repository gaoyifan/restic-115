use log::LevelFilter;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbErr, Schema};

pub mod entities {
    pub mod tokens {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
        #[sea_orm(table_name = "tokens")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: i32,
            pub access_token: String,
            pub refresh_token: String,
            pub updated_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod file_nodes {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
        #[sea_orm(table_name = "file_nodes")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub file_id: String,
            #[sea_orm(indexed)]
            pub parent_id: String,
            #[sea_orm(indexed)]
            pub name: String,
            pub is_dir: bool,
            pub size: i64,
            pub pick_code: String,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }
}

// =========================================================================
// Database initialization
// =========================================================================

pub async fn init_db(db_url: &str) -> Result<DatabaseConnection, DbErr> {
    let mut opt = ConnectOptions::new(db_url);
    opt.sqlx_logging_level(LevelFilter::Debug);
    let db = Database::connect(opt).await?;

    // Enable SQLite performance optimizations
    db.execute(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA cache_size=-256000; -- ~256MB
         PRAGMA temp_store=MEMORY;
         PRAGMA mmap_size=30000000000;",
    ))
    .await?;

    // Create tables if they don't exist
    let builder = db.get_database_backend();
    let schema = Schema::new(builder);

    let tables = [
        builder.build(
            schema
                .create_table_from_entity(entities::tokens::Entity)
                .if_not_exists(),
        ),
        builder.build(
            schema
                .create_table_from_entity(entities::file_nodes::Entity)
                .if_not_exists(),
        ),
    ];

    for stmt in tables {
        db.execute(stmt).await?;
    }

    // Create indexes from entity definitions (#[sea_orm(indexed)] attributes)
    // create_index_from_entity generates CREATE INDEX statements, but doesn't support IF NOT EXISTS,
    // so we ignore "already exists" errors.
    for index_stmt in schema.create_index_from_entity(entities::file_nodes::Entity) {
        let sql = builder.build(&index_stmt);
        if let Err(e) = db.execute(sql).await {
            // Ignore "index already exists" errors (SQLite error code for this)
            let err_str = e.to_string();
            if !err_str.contains("already exists") {
                return Err(e);
            }
        }
    }

    Ok(db)
}
