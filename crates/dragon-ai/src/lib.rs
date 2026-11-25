//! dragon-ai: Motor de Inteligencia Artificial y Búsqueda Semántica de Dragon Radar Tool.
//! Provee embeddings ultra-rápidos basados en Model2Vec sobre CPU (<1ms por texto),
//! almacén vectorial contiguo con escaneo SIMD/Rayon y fusión híbrida RRF con Tantivy.

pub mod hybrid;
pub mod model;
pub mod vector_store;

pub use hybrid::{HybridHit, HybridRanker};
pub use model::{cosine_similarity, l2_normalize, DragonEmbedder, EMBEDDING_DIM};
pub use vector_store::{VectorHit, VectorStore, VectorUnit};
