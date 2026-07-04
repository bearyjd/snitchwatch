// THROWAWAY SPIKE library target.
//
// Exists so an integration test under tests/ can link against the cxx-qt
// generated code — this is the exact scenario of cxx-qt issue #770, where
// generated C++/QML-registration symbols fail to link into a separate test
// target.
pub mod cxxqt_object;
