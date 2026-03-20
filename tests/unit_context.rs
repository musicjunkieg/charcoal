//! Tests for contextual scoring orchestration.

#[test]
fn find_most_similar_posts_returns_top_n() {
    use charcoal::scoring::context::find_most_similar_posts;

    let user_embedding = vec![1.0, 0.0, 0.0];
    let target_posts = vec![
        ("post1".to_string(), vec![0.9, 0.1, 0.0]),   // high sim
        ("post2".to_string(), vec![0.0, 1.0, 0.0]),   // low sim
        ("post3".to_string(), vec![0.8, 0.2, 0.1]),   // medium sim
        ("post4".to_string(), vec![0.95, 0.05, 0.0]), // highest sim
    ];

    let top = find_most_similar_posts(&user_embedding, &target_posts, 2);
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].0, "post4"); // highest first
    assert_eq!(top[1].0, "post1"); // second highest
}

#[test]
fn find_most_similar_posts_returns_empty_for_no_posts() {
    use charcoal::scoring::context::find_most_similar_posts;
    let user_embedding = vec![1.0, 0.0, 0.0];
    let target_posts: Vec<(String, Vec<f64>)> = vec![];
    let top = find_most_similar_posts(&user_embedding, &target_posts, 5);
    assert!(top.is_empty());
}

#[test]
fn find_most_similar_posts_respects_limit() {
    use charcoal::scoring::context::find_most_similar_posts;
    let user_embedding = vec![1.0, 0.0, 0.0];
    let target_posts = vec![
        ("p1".to_string(), vec![0.9, 0.1, 0.0]),
        ("p2".to_string(), vec![0.8, 0.2, 0.0]),
        ("p3".to_string(), vec![0.7, 0.3, 0.0]),
    ];
    let top = find_most_similar_posts(&user_embedding, &target_posts, 1);
    assert_eq!(top.len(), 1);
}

#[test]
fn find_most_similar_posts_returns_all_when_fewer_than_limit() {
    use charcoal::scoring::context::find_most_similar_posts;
    let user_embedding = vec![1.0, 0.0, 0.0];
    let target_posts = vec![("p1".to_string(), vec![0.9, 0.1, 0.0])];
    let top = find_most_similar_posts(&user_embedding, &target_posts, 5);
    assert_eq!(top.len(), 1);
}

#[test]
fn find_best_matching_user_post_returns_closest() {
    use charcoal::scoring::context::find_best_matching_user_post;

    // Protected user's posts with embeddings
    let user_posts = vec![
        ("I love fat liberation".to_string(), vec![0.9, 0.1, 0.0]),
        (
            "Great a cappella rehearsal today".to_string(),
            vec![0.0, 0.9, 0.1],
        ),
        ("DEI work matters".to_string(), vec![0.1, 0.0, 0.9]),
    ];

    // Target post embedding is closest to the fat liberation post
    let target_embedding = vec![0.85, 0.15, 0.0];
    let result = find_best_matching_user_post(&target_embedding, &user_posts);
    assert!(result.is_some());
    assert_eq!(result.unwrap(), "I love fat liberation");
}

#[test]
fn find_best_matching_user_post_returns_none_for_empty() {
    use charcoal::scoring::context::find_best_matching_user_post;

    let user_posts: Vec<(String, Vec<f64>)> = vec![];
    let target_embedding = vec![1.0, 0.0, 0.0];
    assert!(find_best_matching_user_post(&target_embedding, &user_posts).is_none());
}
